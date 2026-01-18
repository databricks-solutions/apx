use clap::Args;
use crate::cli::run_cli_async;
use crate::mcp::server::{build_server, AppContext};

#[derive(Args)]
pub struct McpArgs {}

pub async fn run(_args: McpArgs) -> i32 {
    run_cli_async(|| async {
        let app_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let server = build_server(AppContext { app_dir });
        server
            .run_stdio()
            .await
            .map_err(|e| format!("MCP server error: {e}"))
    })
    .await
}
