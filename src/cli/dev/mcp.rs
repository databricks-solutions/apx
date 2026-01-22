use clap::Args;
use crate::cli::run_cli_async;
use crate::cli::components::new_cache_state;
use crate::mcp::server::{build_server, AppContext};
use crate::databricks_sdk_doc::{SDKDocIndex, SDKSource};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Args)]
pub struct McpArgs {}

pub async fn run(_args: McpArgs) -> i32 {
    run_cli_async(|| async {
        let app_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        // Initialize SDK doc index
        tracing::info!("Initializing Databricks SDK documentation index");
        let sdk_doc_index = match SDKDocIndex::new() {
            Ok(mut index) => {
                // Try to bootstrap the index
                tracing::info!("Bootstrapping SDK docs (this may download SDK if not cached)");
                let bootstrap_result = index.bootstrap(&SDKSource::DatabricksSdkPython).await;
                match bootstrap_result {
                    Ok(true) => {
                        tracing::info!("SDK docs indexed successfully");
                        Some(index)
                    }
                    Ok(false) => {
                        tracing::info!("SDK docs already indexed");
                        Some(index)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to bootstrap SDK docs: {}. The docs tool will not be available.", e);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to initialize SDK doc index: {}. The docs tool will not be available.", e);
                None
            }
        };

        // Create cache state for background population
        let cache_state = new_cache_state();

        let server = build_server(AppContext {
            app_dir,
            sdk_doc_index: Arc::new(Mutex::new(sdk_doc_index)),
            cache_state,
        });

        server
            .run_stdio()
            .await
            .map_err(|e| format!("MCP server error: {e}"))
    })
    .await
}
