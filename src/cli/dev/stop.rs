use clap::Args;
use std::path::{Path, PathBuf};

use crate::cli::run_cli_async;
use crate::dev::client::stop as stop_server;
use crate::dev::common::{lock_path, read_lock, remove_lock};
use crate::dev::process::ProcessManager;
use tracing::{debug, warn};

#[derive(Args, Debug, Clone)]
pub struct StopArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: StopArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: StopArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    stop_dev_server(&app_dir).await
}

/// Stop the dev server for the given app directory.
pub async fn stop_dev_server(app_dir: &Path) -> Result<(), String> {
    let lock_path = lock_path(app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        println!("No dev server lockfile found.");
        return Ok(());
    }

    let lock = read_lock(&lock_path)?;
    debug!(port = lock.port, pid = lock.pid, "Loaded dev server lockfile.");

    // Try graceful shutdown first via HTTP request
    match stop_server(lock.port).await {
        Ok(()) => {
            debug!("Dev server stopped gracefully via HTTP.");
            println!("Dev server stopped.");
            return Ok(());
        }
        Err(err) => {
            warn!(error = %err, "Graceful stop failed, falling back to process kill.");
        }
    }

    // Fall back to killing the process tree if graceful stop failed
    let kill_result = ProcessManager::kill_process_tree(lock.pid, "dev-server");
    match kill_result {
        Ok(()) => {
            debug!("Dev server process tree killed; removing lockfile.");
            remove_lock(&lock_path)?;
            println!("Dev server stopped.");
            Ok(())
        }
        Err(err) => {
            warn!(error = %err, pid = lock.pid, "Failed to kill dev server process tree.");
            remove_lock(&lock_path)?;
            println!("Dev server already stopped.");
            Ok(())
        }
    }
}
