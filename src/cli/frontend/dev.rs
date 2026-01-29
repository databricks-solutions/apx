use clap::Args;
use std::path::{Path, PathBuf};
use tokio::process::Child;
use tracing::debug;

use crate::bun_binary_path;
use crate::cli::run_cli_async;
use crate::common::{read_project_metadata, write_metadata_file};

use super::common::prepare_frontend_args;

/// Environment variables required for frontend dev mode
const DEV_REQUIRED_ENV_VARS: &[&str] = &[
    "APX_FRONTEND_PORT",
    "APX_DEV_SERVER_PORT",
    "APX_DEV_SERVER_HOST",
    "APX_DEV_TOKEN",
];

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
    // Check required environment variables
    let missing_vars: Vec<&str> = DEV_REQUIRED_ENV_VARS
        .iter()
        .filter(|var| std::env::var(var).is_err())
        .copied()
        .collect();

    if !missing_vars.is_empty() {
        eprintln!("⚠️  Note: This command is intended for internal use by the dev server.");
        eprintln!("   Use `apx dev start` to run the full development environment.\n");
        return Err(format!(
            "Missing required environment variables: {}",
            missing_vars.join(", ")
        ));
    }

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
    // Generate metadata file FIRST
    debug!("Generating metadata file before starting frontend dev server");
    let metadata = read_project_metadata(app_dir)?;
    write_metadata_file(app_dir, &metadata)?;

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
