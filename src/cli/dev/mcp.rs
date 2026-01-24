use clap::Args;
use crate::cli::run_cli_async;
use crate::cli::components::new_cache_state;
use crate::mcp::server::{build_server, AppContext, IndexState};
use crate::databricks_sdk_doc::SDKSource;
use crate::search::docs_index::SDKDocsIndex;
use crate::search::embedder::Embedder;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{broadcast, Mutex};

#[derive(Args)]
pub struct McpArgs {}

/// Spawn SDK doc indexing as a background task
fn spawn_sdk_indexing(
    sdk_doc_index: Arc<Mutex<Option<SDKDocsIndex>>>,
    index_state: IndexState,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    tokio::spawn(async move {
        tracing::info!("Initializing Databricks SDK documentation index");
        
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
            Some(Ok(idx)) => idx,
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

        // Bootstrap the index
        tracing::info!("Bootstrapping SDK docs (this may download SDK if not cached)");
        let bootstrap_result = tokio::select! {
            result = index.bootstrap(&SDKSource::DatabricksSdkPython) => Some(result),
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown signal received during SDK doc bootstrapping");
                None
            }
        };

        match bootstrap_result {
            Some(Ok(true)) => {
                tracing::info!("SDK docs indexed successfully");
                *sdk_doc_index.lock().await = Some(index);
            }
            Some(Ok(false)) => {
                tracing::info!("SDK docs already indexed");
                *sdk_doc_index.lock().await = Some(index);
            }
            Some(Err(e)) => {
                tracing::warn!("Failed to bootstrap SDK docs: {}. The docs tool will not be available.", e);
            }
            None => {
                // Shutdown during bootstrap
            }
        }

        // Mark SDK indexing as complete
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

        // Initialize embedder for component search
        let embedder = match Embedder::new() {
            Ok(emb) => Arc::new(emb),
            Err(e) => {
                tracing::error!("Failed to initialize embedder: {}. Component search will not work.", e);
                return Err(format!("Failed to initialize embedder: {}", e));
            }
        };

        // Spawn SDK indexing as background task
        let sdk_doc_index = Arc::new(Mutex::new(None));
        spawn_sdk_indexing(
            Arc::clone(&sdk_doc_index),
            index_state.clone(),
            shutdown_tx.subscribe(),
        );

        let server = build_server(AppContext {
            app_dir,
            sdk_doc_index,
            cache_state,
            embedder,
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
