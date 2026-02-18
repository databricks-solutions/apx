use std::path::Path;

use crate::common::{
    BunCommand, OutputMode, emit, ensure_entrypoint_deps, run_preflight_checks, spinner,
};
use crate::download::resolve_uv;
use crate::frontend::prepare_frontend_args;
use tracing::debug;

/// Run type checking (tsc + ty) in parallel for the given app directory.
pub async fn run_check(app_dir: &Path, mode: OutputMode) -> Result<(), String> {
    // Run preflight checks (installs deps if needed)
    let preflight = run_preflight_checks(app_dir).await?;
    let has_ui = preflight.has_ui;

    // Generate route tree (must complete before tsc) — only for UI projects
    if has_ui {
        generate_route_tree(app_dir, mode).await?;
    }

    // Spinner for the parallel type-check phase (CLI only)
    let check_spinner = if mode == OutputMode::Interactive {
        let sp = spinner("Running type checks...");
        Some(sp)
    } else {
        eprintln!("Running type checks...");
        None
    };

    // Run tsc -b --incremental in one tokio thread — only for UI projects
    let tsc_task = if has_ui {
        let bun = BunCommand::new().await?;
        let app_dir_clone = app_dir.to_path_buf();
        Some(tokio::spawn(async move {
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
        }))
    } else {
        None
    };

    // Run ty check in another thread — always
    let app_dir_clone = app_dir.to_path_buf();
    let uv_path = resolve_uv().await?.path;
    let ty_task = tokio::spawn(async move {
        debug!("Running ty check.");
        let output = tokio::process::Command::new(&uv_path)
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

    // Await results
    let tsc_result = if let Some(task) = tsc_task {
        Some(
            task.await
                .map_err(|err| format!("Failed to join tsc task: {err}"))?,
        )
    } else {
        None
    };

    let ty_result = ty_task
        .await
        .map_err(|err| format!("Failed to join ty task: {err}"))?;

    // Clear the spinner before printing results
    if let Some(sp) = check_spinner {
        sp.finish_and_clear();
    }

    let mut errors = Vec::new();

    if let Some(tsc_result) = tsc_result {
        let tsc_result = tsc_result?;
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
    }

    let ty_result = ty_result?;
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

    let bun = BunCommand::new().await?;
    debug!(
        bun_path = %bun.path().display(),
        entrypoint = %entrypoint.display(),
        ?args,
        app_dir = %app_dir.display(),
        "Running route tree generation"
    );
    let output = bun
        .tokio_command_with_node_path(app_dir)
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
        let exit_code = output
            .status
            .code()
            .map_or("signal".into(), |c| c.to_string());
        return Err(format!(
            "Route tree generation failed (exit {exit_code}):\n\
             bun: {bun_path}\n\
             entrypoint: {entrypoint}\n\
             args: {args:?}\n\
             app_dir: {app_dir}\n\
             stdout:\n{stdout}\n\
             stderr:\n{stderr}",
            bun_path = bun.path().display(),
            entrypoint = entrypoint.display(),
            app_dir = app_dir.display(),
        ));
    }

    if let Some(sp) = route_spinner {
        sp.finish_with_message("Route tree generated");
    } else {
        eprintln!("Route tree generated");
    }
    Ok(())
}
