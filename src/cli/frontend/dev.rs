use std::path::Path;
use tokio::process::Child;

use crate::bun_binary_path;

use super::common::prepare_frontend_args;

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
