use crate::context::{AppContext, SdkIndexParams};
use apx_core::databricks_sdk_doc::SDKSource;
use apx_core::search::ComponentIndex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::broadcast;

/// Initialize all indexes in background (component index, then SDK docs index)
///
/// All database operations use synchronous SQLite calls wrapped in spawn_blocking.
///
/// This is called when the MCP server starts.
pub fn init_all_indexes(
    ctx: &AppContext,
    mut shutdown_rx: broadcast::Receiver<()>,
    sdk_params: Option<SdkIndexParams>,
) {
    let cache_state = ctx.cache_state.clone();
    let index_state = ctx.index_state.clone();

    // Check for legacy LanceDB directory
    apx_core::search::common::check_legacy_lancedb();

    tokio::spawn(async move {
        // Mark as running
        {
            let mut guard = cache_state.lock().await;
            guard.is_running = true;
        }

        // ============================================
        // Phase 1: Component Index (ensure exists, skip project-specific sync)
        // ============================================
        tracing::info!("Ensuring component search index exists on MCP start");

        let ensure_result = tokio::select! {
            result = tokio::task::spawn_blocking(ensure_search_index) => {
                Some(result.unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {e}"))))
            },
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received during search index check, stopping");
                None
            }
        };

        if let Some(Err(e)) = ensure_result {
            tracing::warn!("Failed to ensure search index: {}", e);
        }

        // Mark component indexing as complete
        index_state.component_indexed.store(true, Ordering::SeqCst);
        index_state.component_ready.notify_waiters();
        tracing::debug!("Component index ready");

        // ============================================
        // Phase 2: SDK Docs Index (after component index)
        // ============================================
        if let Some(params) = sdk_params {
            tracing::info!("Initializing Databricks SDK documentation index");

            let version = match params.sdk_version {
                Some(v) => {
                    tracing::debug!("Using pre-computed SDK version: {}", v);
                    v
                }
                None => {
                    tracing::warn!(
                        "Databricks SDK not installed. The docs tool will not be available."
                    );
                    index_state.sdk_indexed.store(true, Ordering::SeqCst);
                    index_state.sdk_ready.notify_waiters();

                    // Mark as done
                    let mut guard = cache_state.lock().await;
                    guard.is_running = false;
                    return;
                }
            };

            // Create SDK docs index (sync, but cheap)
            let mut index = match apx_core::search::docs_index::SDKDocsIndex::new() {
                Ok(idx) => {
                    tracing::debug!("SDKDocsIndex created successfully");
                    idx
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to initialize SDK doc index: {}. The docs tool will not be available.",
                        e
                    );
                    index_state.sdk_indexed.store(true, Ordering::SeqCst);
                    index_state.sdk_ready.notify_waiters();

                    let mut guard = cache_state.lock().await;
                    guard.is_running = false;
                    return;
                }
            };

            // Bootstrap the index (async: download + sync: build)
            tracing::info!("Bootstrapping SDK docs (this may download SDK if not cached)");
            let bootstrap_start = std::time::Instant::now();
            let bootstrap_result = tokio::select! {
                result = index.bootstrap_with_version(&SDKSource::DatabricksSdkPython, &version) => Some(result),
                _ = shutdown_rx.recv() => {
                    tracing::info!("Shutdown signal received during SDK doc bootstrapping");
                    None
                }
            };
            tracing::debug!("SDK bootstrap completed in {:?}", bootstrap_start.elapsed());

            match bootstrap_result {
                Some(Ok(true)) => {
                    tracing::info!("SDK docs indexed successfully");
                    *params.sdk_doc_index.lock().await = Some(index);
                }
                Some(Ok(false)) => {
                    tracing::info!("SDK docs already indexed");
                    *params.sdk_doc_index.lock().await = Some(index);
                }
                Some(Err(e)) => {
                    tracing::warn!(
                        "Failed to bootstrap SDK docs: {}. The docs tool will not be available.",
                        e
                    );
                }
                None => {
                    tracing::debug!("Shutdown during SDK bootstrap");
                }
            }

            // Mark SDK indexing as complete
            index_state.sdk_indexed.store(true, Ordering::SeqCst);
            index_state.sdk_ready.notify_waiters();
            tracing::debug!("SDK doc index ready");
        } else {
            // No SDK params, mark as ready immediately
            index_state.sdk_indexed.store(true, Ordering::SeqCst);
            index_state.sdk_ready.notify_waiters();
        }

        // Mark as done
        {
            let mut guard = cache_state.lock().await;
            guard.is_running = false;
        }
    });
}

/// Rebuild the search index from registry.json files (sync)
pub fn rebuild_search_index() -> Result<(), String> {
    let index = ComponentIndex::new()?;
    index.build_index_from_registries()
}

/// Ensure search index exists and is valid, build/rebuild if needed (sync)
fn ensure_search_index() -> Result<(), String> {
    let index = ComponentIndex::new()?;

    match index.validate_index() {
        Ok(true) => {
            tracing::debug!("Search index validated successfully");
            Ok(())
        }
        Ok(false) => {
            tracing::info!("Search index not found, building from registry indexes");
            index.build_index_from_registries()
        }
        Err(e) => {
            tracing::warn!("Search index corrupted ({}), rebuilding...", e);
            index.build_index_from_registries()
        }
    }
}

/// Wait for an index to be ready with timeout (15 seconds)
pub async fn wait_for_index_ready(
    ready_notify: &Notify,
    is_ready: &AtomicBool,
    index_name: &str,
) -> Result<(), String> {
    const TIMEOUT_SECS: u64 = 15;

    // Check if already ready
    if is_ready.load(Ordering::SeqCst) {
        return Ok(());
    }

    tracing::debug!(
        "Waiting up to {}s for {} index to be ready",
        TIMEOUT_SECS,
        index_name
    );

    // Wait with timeout
    match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), ready_notify.notified()).await {
        Ok(_) => {
            tracing::debug!("{} index is now ready", index_name);
            Ok(())
        }
        Err(_) => {
            tracing::warn!(
                "{} index not ready after {}s timeout",
                index_name,
                TIMEOUT_SECS
            );
            Err(format!(
                "{index_name} index is not yet ready, please rerun the query in 5 seconds"
            ))
        }
    }
}
