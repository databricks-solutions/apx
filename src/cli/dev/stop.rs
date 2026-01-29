use clap::Args;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::run_cli_async;
use crate::common::{format_elapsed_ms, spinner};
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

    stop_dev_server(&app_dir).await?;
    Ok(())
}

/// Stop the dev server for the given app directory.
/// Returns true if a server was found and stopped, false if no server was running.
pub async fn stop_dev_server(app_dir: &Path) -> Result<bool, String> {
    let lock_path = lock_path(app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        println!("⚠️  No dev server running\n");
        return Ok(false);
    }

    let lock = read_lock(&lock_path)?;
    debug!(
        port = lock.port,
        pid = lock.pid,
        "Loaded dev server lockfile."
    );

    let start_time = Instant::now();
    let stop_spinner = spinner("Stopping dev server...");

    // Try graceful shutdown first via HTTP request
    match stop_server(lock.port).await {
        Ok(()) => {
            debug!("Dev server stopped gracefully via HTTP.");
            stop_spinner.finish_and_clear();
            println!(
                "✅ Dev server stopped in {}\n",
                format_elapsed_ms(start_time)
            );
            return Ok(true);
        }
        Err(err) => {
            warn!(error = %err, "Graceful stop failed, falling back to process kill.");
        }
    }

    // Fall back to killing the process tree if graceful stop failed
    let kill_result = ProcessManager::kill_process_tree(lock.pid, "dev-server");
    stop_spinner.finish_and_clear();
    match kill_result {
        Ok(()) => {
            debug!("Dev server process tree killed; removing lockfile.");
            remove_lock(&lock_path)?;
            println!(
                "✅ Dev server stopped in {}\n",
                format_elapsed_ms(start_time)
            );
            Ok(true)
        }
        Err(err) => {
            warn!(error = %err, pid = lock.pid, "Failed to kill dev server process tree.");
            remove_lock(&lock_path)?;
            println!("✅ Dev server already stopped\n");
            Ok(true)
        }
    }
}
