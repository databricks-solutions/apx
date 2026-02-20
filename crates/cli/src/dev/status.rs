use clap::Args;
use std::path::PathBuf;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::dev::client::status as get_status;
use apx_core::dev::common::{lock_path, read_lock};
use tracing::debug;

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: StatusArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: StatusArgs) -> Result<(), String> {
    let app_dir = find_app_dir(args.app_path)?;

    let lock_path = lock_path(&app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");

    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        println!("Dev Server: not running");
        return Ok(());
    }

    let lock = read_lock(&lock_path)?;
    debug!(
        port = lock.port,
        pid = lock.pid,
        "Loaded dev server lockfile."
    );

    // Query the health endpoint
    match get_status(lock.port).await {
        Ok(status) => {
            println!(
                "🚀 Dev server is running at http://{}:{}",
                apx_common::hosts::BROWSER_HOST,
                lock.port
            );
            println!("Frontend: {}", status.frontend_status);
            println!("Backend: {}", status.backend_status);
            Ok(())
        }
        Err(err) => {
            debug!(error = %err, "Failed to get status from dev server.");
            println!("Dev Server: running (but unreachable)");
            println!("Error: {err}");
            Err(err.to_string())
        }
    }
}
