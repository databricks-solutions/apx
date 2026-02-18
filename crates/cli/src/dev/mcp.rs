use crate::run_cli_async_helper;
use apx_core::components::new_cache_state;
use apx_core::databricks_sdk_doc::fetch_latest_sdk_version;
use apx_core::interop::get_databricks_sdk_version;
use apx_db::DevDb;
use apx_mcp::context::{AppContext, IndexState, SdkIndexParams};
use apx_mcp::server::run_server;
use clap::Args;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

#[derive(Args, Debug)]
pub struct McpArgs {}

pub async fn run(_args: McpArgs) -> i32 {
    run_cli_async_helper(|| async {
        // Create shutdown channel
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        // Create index state
        let index_state = IndexState::new();

        // Create cache state for background population
        let cache_state = new_cache_state();

        // Get SDK version via subprocess before spawning async task
        const DEFAULT_SDK_VERSION: &str = "0.89.0";
        let sdk_version = match get_databricks_sdk_version() {
            Ok(Some(v)) => {
                tracing::info!("Found Databricks SDK version: {}", v);
                v
            }
            Ok(None) | Err(_) => {
                tracing::info!("SDK not detected locally, fetching latest version from GitHub");
                match fetch_latest_sdk_version().await {
                    Ok(v) => {
                        tracing::info!("Latest SDK version from GitHub: {}", v);
                        v
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to fetch latest SDK version: {}. Using default {}",
                            e,
                            DEFAULT_SDK_VERSION
                        );
                        DEFAULT_SDK_VERSION.to_string()
                    }
                }
            }
        };

        // Create SDK doc index holder and params
        let sdk_doc_index = Arc::new(Mutex::new(None));
        let sdk_params = SdkIndexParams {
            sdk_version,
            sdk_doc_index: Arc::clone(&sdk_doc_index),
        };

        let dev_db = DevDb::open()
            .await
            .map_err(|e| format!("Failed to open dev database: {e}"))?;

        let ctx = AppContext {
            dev_db,
            sdk_doc_index,
            cache_state,
            index_state,
            shutdown_tx: shutdown_tx.clone(),
        };

        run_server(ctx, Some(sdk_params)).await
    })
    .await
}
