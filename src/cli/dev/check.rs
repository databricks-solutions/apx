use clap::Args;
use std::path::PathBuf;

use crate::cli::run_cli_async;
use crate::common::{BunCommand, run_preflight_checks};
use tokio::process::Command as TokioCommand;
use tracing::debug;

#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: CheckArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

pub async fn run_inner(args: CheckArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Run preflight checks (installs deps if needed)
    run_preflight_checks(&app_dir).await?;

    // Run tsc -b --incremental in one tokio thread
    let bun = BunCommand::new()?;
    let app_dir_clone = app_dir.clone();
    let tsc_task = tokio::spawn(async move {
        debug!(bun_path = %bun.path().display(), "Running tsc -b --incremental.");
        let output = bun
            .tokio_command()
            .arg("run")
            .arg("tsc")
            .arg("-b")
            .arg("--incremental")
            .current_dir(&app_dir_clone)
            .output()
            .await
            .map_err(|err| format!("Failed to run tsc: {err}"))?;

        Ok::<(bool, String, String), String>((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    });

    // Run ty check in another thread
    let app_dir_clone = app_dir.clone();
    let ty_task = tokio::spawn(async move {
        debug!("Running ty check.");
        let output = TokioCommand::new("uv")
            .arg("run")
            .arg("ty")
            .arg("check")
            .arg(".")
            .current_dir(&app_dir_clone)
            .output()
            .await
            .map_err(|err| format!("Failed to run ty check: {err}"))?;

        Ok::<(bool, String, String), String>((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    });

    // Wait for both tasks to complete
    let (tsc_result, ty_result) = tokio::try_join!(tsc_task, ty_task)
        .map_err(|err| format!("Failed to join tasks: {err}"))?;

    let tsc_result = tsc_result?;
    let ty_result = ty_result?;

    // Collect errors
    let mut errors = Vec::new();

    if !tsc_result.0 {
        println!("❌ [tsc] TypeScript compilation failed");
        // Combine stdout and stderr for tsc (errors typically in stderr)
        let combined_output = if !tsc_result.2.is_empty() && !tsc_result.1.is_empty() {
            format!("{}\n{}", tsc_result.1, tsc_result.2)
        } else if !tsc_result.2.is_empty() {
            tsc_result.2.clone()
        } else if !tsc_result.1.is_empty() {
            tsc_result.1.clone()
        } else {
            String::new()
        };

        if !combined_output.is_empty() {
            println!("{combined_output}");
        }

        errors.push(format!(
            "[tsc] TypeScript compilation failed: {}",
            if combined_output.is_empty() {
                "no output"
            } else {
                &combined_output
            }
        ));
    } else {
        println!("✅ [tsc] TypeScript compilation succeeded");
    }

    if !ty_result.0 {
        println!("❌ [ty] Python type check failed");
        // Combine stdout and stderr for ty (errors typically in stdout)
        let combined_output = if !ty_result.1.is_empty() && !ty_result.2.is_empty() {
            format!("{}\n{}", ty_result.1, ty_result.2)
        } else if !ty_result.1.is_empty() {
            ty_result.1.clone()
        } else if !ty_result.2.is_empty() {
            ty_result.2.clone()
        } else {
            String::new()
        };

        if !combined_output.is_empty() {
            println!("{combined_output}");
        }

        errors.push(format!(
            "[ty] Python type check failed: {}",
            if combined_output.is_empty() {
                "no output"
            } else {
                &combined_output
            }
        ));
    } else {
        println!("✅ [ty] Python type check succeeded");
    }

    // If there are errors, raise them
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }

    // If no errors, just move forward
    Ok(())
}
