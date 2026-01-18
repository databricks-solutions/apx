use clap::Args;
use crate::mcp::server::{build_server, AppContext};

#[derive(Args)]
pub struct McpArgs {}

pub fn run(_args: McpArgs) -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create tokio runtime: {}", e);
            return 1;
        }
    };

    rt.block_on(async {
        let app_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let server = build_server(AppContext { app_dir });
        match server.run_stdio().await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("MCP server error: {}", e);
                1
            }
        }
    })
}
