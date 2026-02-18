use crate::context::{AppContext, SdkIndexParams};
use apx_core::databricks_sdk_doc::SDKSource;
use apx_core::search::ComponentIndex;
use apx_db::SqlitePool;
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
    let pool = ctx.dev_db.pool().clone();

    // Check for legacy LanceDB directory
    apx_core::search::common::check_legacy_paths();

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
            result = ensure_search_index(pool.clone()) => {
                Some(result)
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

            let version = params.sdk_version;
            tracing::debug!("Using SDK version: {}", version);

            // Create SDK docs index (async)
            let mut index = apx_core::search::docs_index::SDKDocsIndex::new(pool.clone());
            tracing::debug!("SDKDocsIndex created successfully");

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

/// Rebuild the search index from registry.json files (async)
pub async fn rebuild_search_index(pool: SqlitePool) -> Result<(), String> {
    let index = ComponentIndex::new(pool);
    index.build_index_from_registries().await
}

/// Ensure search index exists and is valid, build/rebuild if needed (async)
async fn ensure_search_index(pool: SqlitePool) -> Result<(), String> {
    let index = ComponentIndex::new(pool);

    match index.validate_index().await {
        Ok(true) => {
            tracing::debug!("Search index validated successfully");
            Ok(())
        }
        Ok(false) => {
            tracing::info!("Search index not found, building from registry indexes");
            index.build_index_from_registries().await
        }
        Err(e) => {
            tracing::warn!("Search index corrupted ({}), rebuilding...", e);
            index.build_index_from_registries().await
        }
    }
}

/// Wait for an index to be ready with timeout (15 seconds).
///
/// Returns `true` if the index was already ready, `false` if we had to wait.
/// Returns `Err` if the timeout expired before the index became ready.
pub async fn wait_for_index_ready(
    ready_notify: &Notify,
    is_ready: &AtomicBool,
    index_name: &str,
) -> Result<bool, String> {
    const TIMEOUT_SECS: u64 = 15;

    // Register as a waiter BEFORE checking the flag to avoid a race where
    // notify_waiters() fires between our is_ready check and the notified() call.
    // (notify_waiters does not store a permit, so late registrations miss it.)
    let notified = ready_notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();

    // Fast path: already ready
    if is_ready.load(Ordering::SeqCst) {
        return Ok(true);
    }

    tracing::debug!(
        "Waiting up to {}s for {} index to be ready",
        TIMEOUT_SECS,
        index_name
    );

    // Wait with timeout
    match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), notified).await {
        Ok(_) => {
            tracing::debug!("{} index is now ready", index_name);
            Ok(false)
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
