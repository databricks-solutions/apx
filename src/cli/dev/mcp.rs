use crate::cli::components::new_cache_state;
use crate::cli::run_cli_async;
use crate::interop::get_databricks_sdk_version;
use crate::mcp::server::{AppContext, IndexState, SdkIndexParams, build_server};
use clap::Args;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

#[derive(Args)]
pub struct McpArgs {}

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

        // Create SDK doc index holder and params
        let sdk_doc_index = Arc::new(Mutex::new(None));
        let sdk_params = SdkIndexParams {
            sdk_version,
            sdk_doc_index: Arc::clone(&sdk_doc_index),
        };

        // Build server with SDK params - all indexing happens sequentially in one task
        let server = build_server(
            AppContext {
                app_dir,
                sdk_doc_index,
                cache_state,
                index_state,
                shutdown_tx: shutdown_tx.clone(),
            },
            Some(sdk_params),
        );

        server
            .run_stdio(shutdown_tx)
            .await
            .map_err(|e| format!("MCP server error: {e}"))
    })
    .await
}
