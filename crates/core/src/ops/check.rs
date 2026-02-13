use std::path::Path;

use crate::common::{
    BunCommand, OutputMode, ensure_entrypoint_deps, run_preflight_checks, spinner,
};
use crate::frontend::prepare_frontend_args;
use tokio::process::Command as TokioCommand;
use tracing::debug;

/// Run type checking (tsc + ty) in parallel for the given app directory.
pub async fn run_check(app_dir: &Path, mode: OutputMode) -> Result<(), String> {
    // Run preflight checks (installs deps if needed)
    run_preflight_checks(app_dir).await?;

    // Generate route tree (must complete before tsc)
    generate_route_tree(app_dir, mode).await?;

    // Spinner for the parallel type-check phase (CLI only)
    let check_spinner = if mode == OutputMode::Interactive {
        let sp = spinner("Running type checks...");
        Some(sp)
    } else {
        eprintln!("Running type checks...");
        None
    };

    // Run tsc -b --incremental in one tokio thread
    let bun = BunCommand::new()?;
    let app_dir_clone = app_dir.to_path_buf();
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
    let app_dir_clone = app_dir.to_path_buf();
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

    let (tsc_result, ty_result) = tokio::try_join!(tsc_task, ty_task)
        .map_err(|err| format!("Failed to join tasks: {err}"))?;

    // Clear the spinner before printing results
    if let Some(sp) = check_spinner {
        sp.finish_and_clear();
    }

    let tsc_result = tsc_result?;
    let ty_result = ty_result?;

    let mut errors = Vec::new();

    if !tsc_result.0 {
        emit(mode, "❌ [tsc] TypeScript compilation failed");
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
            emit(mode, &combined_output);
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
        emit(mode, "✅ [tsc] TypeScript compilation succeeded");
    }

    if !ty_result.0 {
        emit(mode, "❌ [ty] Python type check failed");
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
            emit(mode, &combined_output);
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
        emit(mode, "✅ [ty] Python type check succeeded");
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }

    Ok(())
}

async fn generate_route_tree(app_dir: &Path, mode: OutputMode) -> Result<(), String> {
    let route_spinner = if mode == OutputMode::Interactive {
        Some(spinner("Generating route tree..."))
    } else {
        eprintln!("Generating route tree...");
        None
    };

    ensure_entrypoint_deps(app_dir).await?;

    let (entrypoint, args, app_name) = prepare_frontend_args(app_dir, "generate")?;

    let bun = BunCommand::new()?;
    let output = bun
        .tokio_command()
        .arg("run")
        .arg(&entrypoint)
        .args(&args)
        .env("APX_APP_NAME", &app_name)
        .current_dir(app_dir)
        .output()
        .await
        .map_err(|err| format!("Failed to run route tree generation: {err}"))?;

    if !output.status.success() {
        if let Some(sp) = route_spinner {
            sp.finish_and_clear();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Route tree generation failed:\n{stdout}\n{stderr}"));
    }

    if let Some(sp) = route_spinner {
        sp.finish_with_message("Route tree generated");
    } else {
        eprintln!("Route tree generated");
    }
    Ok(())
}

/// Print a message to stdout (Interactive) or stderr (Quiet).
fn emit(mode: OutputMode, msg: &str) {
    match mode {
        OutputMode::Interactive => println!("{msg}"),
        OutputMode::Quiet => eprintln!("{msg}"),
    }
}
