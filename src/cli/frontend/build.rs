use clap::Args;
use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::run_cli_async;
use crate::common::{
    BunCommand, ensure_entrypoint_deps, format_elapsed_ms, run_command_streaming_with_output,
    run_preflight_checks, spinner,
};

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

    // Run preflight checks (installs deps if needed)
    run_preflight_checks(&app_path).await?;

    run_build(&app_path, true).await
}

/// Run frontend in build mode
/// This function is public so it can be used by cli::build
/// If `print_status` is true, prints start/finish messages
pub async fn run_build(app_dir: &Path, print_status: bool) -> Result<(), String> {
    let sp = if print_status {
        Some(spinner("📦 Building frontend..."))
    } else {
        None
    };

    let result = run_build_with_spinner(app_dir, sp.as_ref()).await;

    if let Some(sp) = sp {
        sp.finish_and_clear();
    }

    match result {
        Ok(start_time) => {
            if print_status {
                println!(
                    "✅ Frontend build finished in {}",
                    format_elapsed_ms(start_time)
                );
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Run frontend build with streaming output to a spinner.
/// If no spinner is provided, output is still captured but not displayed in real-time.
/// Returns the elapsed time for the caller to display if needed.
pub async fn run_build_with_spinner(
    app_dir: &Path,
    spinner: Option<&ProgressBar>,
) -> Result<Instant, String> {
    let start_time = Instant::now();

    // Ensure entrypoint.ts dependencies are installed
    ensure_entrypoint_deps(app_dir).await?;

    let (entrypoint, args, app_name) = prepare_frontend_args(app_dir, "build")?;
    let bun = BunCommand::new()?;

    let mut cmd = bun.tokio_command();
    cmd.arg("run")
        .arg(&entrypoint)
        .args(&args)
        .current_dir(app_dir)
        .env("APX_APP_NAME", &app_name);

    // Use streaming if a spinner is provided
    if let Some(sp) = spinner {
        run_command_streaming_with_output(cmd, sp, "📦 Frontend:", "Frontend build failed").await?;
    } else {
        // Fallback to non-streaming for when no spinner is provided
        let output = cmd
            .output()
            .await
            .map_err(|err| format!("Failed to run frontend build: {err}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            let mut error_msg = format!(
                "Frontend build failed with status {}",
                output.status.code().unwrap_or(1)
            );

            if !stderr.is_empty() {
                error_msg.push_str(&format!("\n\nStderr:\n{}", stderr.trim()));
            }

            if !stdout.is_empty() {
                error_msg.push_str(&format!("\n\nStdout:\n{}", stdout.trim()));
            }

            return Err(error_msg);
        }
    }

    Ok(start_time)
}
