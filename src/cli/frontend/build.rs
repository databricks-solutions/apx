use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use crate::bun_binary_path;

use super::common::prepare_frontend_args;

/// Run frontend in build mode
pub async fn run_build(app_dir: &Path) -> Result<(), String> {
    let (entrypoint, args, app_name) = prepare_frontend_args(app_dir, "build")?;
    let bun_path = bun_binary_path()?;

    let output = Command::new(&bun_path)
        .arg("run")
        .arg(&entrypoint)
        .args(&args)
        .current_dir(app_dir)
        .env("APX_APP_NAME", &app_name)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .await
        .map_err(|err| format!("Failed to run frontend build: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "Frontend build failed with status {}",
            output.status.code().unwrap_or(1)
        ));
    }

    Ok(())
}
