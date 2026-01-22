use clap::Args;
use std::path::{Path, PathBuf};
use tokio::process::Child;

use crate::bun_binary_path;
use crate::cli::run_cli_async;

use super::common::prepare_frontend_args;

#[derive(Args, Debug, Clone)]
pub struct DevArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: DevArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: DevArgs) -> Result<(), String> {
    let app_path = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut child = run_dev(&app_path).await?;
    
    // Wait for the child process
    let status = child
        .wait()
        .await
        .map_err(|err| format!("Failed to wait for frontend dev server: {err}"))?;

    if !status.success() {
        return Err(format!(
            "Frontend dev server exited with status {}",
            status.code().unwrap_or(1)
        ));
    }

    Ok(())
}

/// Run frontend in dev mode
/// Returns the spawned child process for the caller to manage
pub async fn run_dev(app_dir: &Path) -> Result<Child, String> {
    let (entrypoint, args, app_name) = prepare_frontend_args(app_dir, "dev")?;
    let bun_path = bun_binary_path()?;

    let child = tokio::process::Command::new(&bun_path)
        .arg("run")
        .arg(&entrypoint)
        .args(&args)
        .current_dir(app_dir)
        .env("APX_APP_NAME", &app_name)
        .spawn()
        .map_err(|err| format!("Failed to spawn frontend dev server: {err}"))?;

    Ok(child)
}
