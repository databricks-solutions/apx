use clap::Args;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

use crate::bun_binary_path;
use crate::cli::run_cli_async;
use crate::common::format_elapsed_ms;

use super::common::prepare_frontend_args;

#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: BuildArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: BuildArgs) -> Result<(), String> {
    let app_path = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    run_build(&app_path, true).await
}

/// Run frontend in build mode
/// This function is public so it can be used by cli::build
/// If `print_status` is true, prints start/finish messages
pub async fn run_build(app_dir: &Path, print_status: bool) -> Result<(), String> {
    let start_time = Instant::now();

    if print_status {
        println!("📦 Starting frontend build...");
    }

    let (entrypoint, args, app_name) = prepare_frontend_args(app_dir, "build")?;
    let bun_path = bun_binary_path()?;

    let output = Command::new(&bun_path)
        .arg("run")
        .arg(&entrypoint)
        .args(&args)
        .current_dir(app_dir)
        .env("APX_APP_NAME", &app_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| format!("Failed to run frontend build: {err}"))?;

    // Print stdout if there's any output
    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        let mut error_msg = format!(
            "Frontend build failed with status {}",
            output.status.code().unwrap_or(1)
        );

        if !stderr.is_empty() {
            error_msg.push_str(&format!("\n\nError output:\n{}", stderr.trim()));
        }

        if !stdout.is_empty() && stderr.is_empty() {
            // If there's no stderr but there is stdout, it might contain error info
            error_msg.push_str(&format!("\n\nBuild output:\n{}", stdout.trim()));
        }

        return Err(error_msg);
    }

    if print_status {
        println!(
            "✅ Frontend build finished in {}\n",
            format_elapsed_ms(start_time)
        );
    }

    Ok(())
}
