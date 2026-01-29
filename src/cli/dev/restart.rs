use clap::Args;
use std::path::{Path, PathBuf};

use crate::cli::dev::start::spawn_server;
use crate::cli::dev::stop::stop_dev_server;
use crate::cli::run_cli_async;
use crate::dev::common::{lock_path, read_lock};

#[derive(Args, Debug, Clone)]
pub struct RestartArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: RestartArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: RestartArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    restart_dev_server(&app_dir).await?;
    Ok(())
}

/// Restart the dev server for the given app directory.
/// Preserves the port if an existing server is found.
pub async fn restart_dev_server(app_dir: &Path) -> Result<u16, String> {
    let lock_path = lock_path(app_dir);
    let preferred_port = if lock_path.exists() {
        let lock = read_lock(&lock_path)?;
        println!(
            "Found existing dev server at http://localhost:{port}",
            port = lock.port
        );
        stop_dev_server(app_dir).await?;
        Some(lock.port)
    } else {
        None
    };

    let port = spawn_server(app_dir, preferred_port, false, 60).await?;
    println!("✅ Dev server restarted at http://localhost:{port}\n");
    Ok(port)
}
