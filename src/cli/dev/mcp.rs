use clap::Args;
use crate::cli::run_cli_async;
use crate::cli::components::new_cache_state;
use crate::mcp::server::{build_server, AppContext, IndexState};
use crate::databricks_sdk_doc::SDKSource;
use crate::search::docs_index::SDKDocsIndex;
use crate::interop::get_databricks_sdk_version;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{broadcast, Mutex};

#[derive(Args)]
pub struct McpArgs {}

/// Spawn SDK doc indexing as a background task
/// 
/// The `sdk_version` is pre-computed synchronously before spawning to avoid
/// Python GIL issues when calling PyO3 from async context.
fn spawn_sdk_indexing(
    sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
    index_state: IndexState,
    mut shutdown_rx: broadcast::Receiver<()>,
    sdk_version: Option<String>,
) {
    tokio::spawn(async move {
        tracing::info!("Initializing Databricks SDK documentation index");
        
        // Check if SDK version is available (pre-computed)
        let version = match sdk_version {
            Some(v) => {
                tracing::debug!("spawn_sdk_indexing: Using pre-computed SDK version: {}", v);
                v
            }
            None => {
                tracing::warn!("Databricks SDK not installed. The docs tool will not be available.");
                index_state.sdk_indexed.store(true, Ordering::SeqCst);
                index_state.sdk_ready.notify_waiters();
                return;
            }
        };
        
        tracing::debug!("spawn_sdk_indexing: Creating SDKDocsIndex::new()");
        
        // Create index
        let index_result = tokio::select! {
            result = async {
                SDKDocsIndex::new()
            } => Some(result),
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received during SDK doc index initialization");
                None
            }
        };

        let mut index = match index_result {
            Some(Ok(idx)) => {
                tracing::debug!("spawn_sdk_indexing: SDKDocsIndex created successfully");
                idx
            }
            Some(Err(e)) => {
                tracing::warn!("Failed to initialize SDK doc index: {}. The docs tool will not be available.", e);
                index_state.sdk_indexed.store(true, Ordering::SeqCst);
                index_state.sdk_ready.notify_waiters();
                return;
            }
            None => {
                // Shutdown during initialization
                index_state.sdk_indexed.store(true, Ordering::SeqCst);
                index_state.sdk_ready.notify_waiters();
                return;
            }
        };

        // Bootstrap the index with pre-computed version
        tracing::info!("Bootstrapping SDK docs (this may download SDK if not cached)");
        tracing::debug!("spawn_sdk_indexing: Calling index.bootstrap_with_version()");
        let bootstrap_start = std::time::Instant::now();
        let bootstrap_result = tokio::select! {
            result = index.bootstrap_with_version(&SDKSource::DatabricksSdkPython, &version) => Some(result),
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received during SDK doc bootstrapping");
                None
            }
        };
        tracing::debug!("spawn_sdk_indexing: bootstrap() returned after {:?}", bootstrap_start.elapsed());

        match bootstrap_result {
            Some(Ok(true)) => {
                tracing::info!("SDK docs indexed successfully");
                tracing::debug!("spawn_sdk_indexing: Storing index in shared state");
                *sdk_doc_index.lock().await = Some(index);
            }
            Some(Ok(false)) => {
                tracing::info!("SDK docs already indexed");
                tracing::debug!("spawn_sdk_indexing: Storing index in shared state (already indexed)");
                *sdk_doc_index.lock().await = Some(index);
            }
            Some(Err(e)) => {
                tracing::warn!("Failed to bootstrap SDK docs: {}. The docs tool will not be available.", e);
            }
            None => {
                // Shutdown during bootstrap
                tracing::debug!("spawn_sdk_indexing: Shutdown during bootstrap");
            }
        }

        // Mark SDK indexing as complete
        tracing::debug!("spawn_sdk_indexing: Marking SDK indexing as complete and notifying waiters");
        index_state.sdk_indexed.store(true, Ordering::SeqCst);
        index_state.sdk_ready.notify_waiters();
        tracing::debug!("SDK doc index ready");
    });
}

pub async fn run(_args: McpArgs) -> i32 {
    run_cli_async(|| async {
        let app_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        // Create shutdown channel
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        // Create index state
        let index_state = IndexState::new();

        // Create cache state for background population
        let cache_state = new_cache_state();

        // Pre-compute SDK version synchronously before spawning async task
        // This avoids Python GIL issues when calling PyO3 from async context
        let sdk_version = match get_databricks_sdk_version() {
            Ok(version) => {
                if let Some(ref v) = version {
                    tracing::info!("Found Databricks SDK version: {}", v);
                } else {
                    tracing::debug!("Databricks SDK not installed");
                }
                version
            }
            Err(e) => {
                tracing::warn!("Failed to get Databricks SDK version: {}", e);
                None
            }
        };

        // Spawn SDK indexing as background task with pre-computed version
        let sdk_doc_index = Arc::new(Mutex::new(None));
        spawn_sdk_indexing(
            Arc::clone(&sdk_doc_index),
            index_state.clone(),
            shutdown_tx.subscribe(),
            sdk_version,
        );

        let server = build_server(AppContext {
            app_dir,
            sdk_doc_index,
            cache_state,
            index_state,
            shutdown_tx: shutdown_tx.clone(),
        });

        server
            .run_stdio(shutdown_tx)
            .await
            .map_err(|e| format!("MCP server error: {e}"))
    })
    .await
}
