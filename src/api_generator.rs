use notify::{RecursiveMode, Watcher};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::time::SystemTime;
use tokio::process::Command as TokioCommand;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::{Duration, Sleep};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::common::{read_project_metadata, write_metadata_file};
use crate::dev::common::Shutdown;
use crate::interop::generate_openapi_spec;
use crate::openapi;

pub fn generate_openapi(project_root: &Path) -> Result<(), String> {
    let metadata = read_project_metadata(project_root)?;
    let app_slug = metadata.app_slug.clone();
    let app_entrypoint = metadata.app_entrypoint.clone();

    // Ensure _metadata.py exists before importing the Python module
    write_metadata_file(project_root, &metadata)?;

    let (spec_json, app_slug) =
        generate_openapi_spec(project_root, &app_entrypoint, &app_slug)?;

    let api_ts_path = project_root
        .join("src")
        .join(&app_slug)
        .join("ui")
        .join("lib")
        .join("api.ts");

    debug!(
        api_ts_path = %api_ts_path.display(),
        spec_len = spec_json.len(),
        "Resolved OpenAPI output path."
    );

    // Generate TypeScript code from OpenAPI spec
    debug!("Generating TypeScript API client from OpenAPI spec.");
    let ts_code = openapi::generate(&spec_json)?;

    // Ensure the output directory exists
    if let Some(parent) = api_ts_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create api.ts directory: {err}"))?;
    }

    // Write the generated TypeScript code
    fs::write(&api_ts_path, &ts_code)
        .map_err(|err| format!("Failed to write api.ts: {err}"))?;

    debug!(
        api_ts_path = %api_ts_path.display(),
        ts_code_len = ts_code.len(),
        "TypeScript API client generated successfully."
    );

    Ok(())
}

const OPENAPI_WATCH_DEBOUNCE_MS: u64 = 100;

pub fn start_openapi_watcher(
    app_dir: PathBuf,
    mut shutdown_rx: broadcast::Receiver<Shutdown>,
) -> Result<(), String> {
    debug!(
        app_dir = %app_dir.display(),
        "Starting OpenAPI watcher."
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .map_err(|err| format!("Failed to create file watcher: {err}"))?;
    watcher
        .watch(&app_dir, RecursiveMode::Recursive)
        .map_err(|err| format!("Failed to watch app dir: {err}"))?;

    let initial_mtime = latest_python_mtime(&app_dir);

    tokio::spawn(async move {
        let _watcher = watcher;
        // Always run initial generation on startup
        let mut pending = true;
        let mut is_initial_generation = true;
        let mut debounce: Option<Pin<Box<Sleep>>> = {
            info!("Will generate OpenAPI on startup");
            // Start with a short debounce to allow server to fully initialize
            Some(Box::pin(tokio::time::sleep(Duration::from_millis(500))))
        };
        let mut last_mtime = initial_mtime;
        let mut poll_interval = tokio::time::interval(Duration::from_millis(200));

        loop {
            tokio::select! {
                biased;

                // React to shutdown signal
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(Shutdown::Stop { .. }) | Err(_) => {
                            debug!("OpenAPI watcher stopping.");
                            break;
                        }
                    }
                }

                _ = poll_interval.tick() => {
                    let latest = latest_python_mtime(&app_dir);
                    let changed = latest.map_or(false, |modified| {
                        last_mtime.map_or(true, |current| modified > current)
                    });
                    if changed {
                        if !pending {
                            info!("Python change detected, regenerating OpenAPI…");
                        }
                        pending = true;
                        debounce = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                            OPENAPI_WATCH_DEBOUNCE_MS,
                        ))));
                        last_mtime = latest;
                    }
                }
                maybe = rx.recv() => {
                    let Some(result) = maybe else {
                        debug!("OpenAPI watcher channel closed.");
                        break;
                    };
                    match result {
                        Ok(event) => {
                            let mut has_python_change = false;
                            for path in &event.paths {
                                if is_ignored_path(path) || !is_python_path(path) {
                                    continue;
                                }
                                has_python_change = true;
                                if let Ok(metadata) = fs::metadata(path) {
                                    if let Ok(modified) = metadata.modified() {
                                        if last_mtime.map_or(true, |current| modified > current) {
                                            last_mtime = Some(modified);
                                        }
                                    }
                                }
                            }
                            if has_python_change {
                                if !pending {
                                    info!("Python change detected, regenerating OpenAPI…");
                                }
                                pending = true;
                                debounce = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                                    OPENAPI_WATCH_DEBOUNCE_MS,
                                ))));
                            }
                        }
                        Err(err) => {
                            warn!("OpenAPI watcher error: {err}");
                        }
                    }
                }
                _ = async { debounce.as_mut().unwrap().await }, if debounce.is_some() => {
                    debounce = None;
                    if pending {
                        pending = false;
                        let is_initial = is_initial_generation;
                        is_initial_generation = false;
                        
                        if is_initial {
                            info!("Running initial OpenAPI generation...");
                        } else {
                            info!("Running OpenAPI regeneration...");
                        }
                        
                        let output = TokioCommand::new("uv")
                            .arg("run")
                            .arg("apx")
                            .arg("__generate_openapi")
                            .arg("--app-dir")
                            .arg(&app_dir)
                            .current_dir(&app_dir)
                            .output();
                        let output = tokio::time::timeout(Duration::from_secs(30), output).await;
                        match output {
                            Ok(Ok(result)) if result.status.success() => {
                                let stdout = String::from_utf8_lossy(&result.stdout);
                                if is_initial {
                                    if stdout.contains("regenerated") {
                                        info!("Initial OpenAPI generated successfully");
                                    } else {
                                        info!("Initial OpenAPI generation complete (unchanged)");
                                    }
                                } else if stdout.contains("regenerated") {
                                    info!("OpenAPI regenerated successfully");
                                } else {
                                    info!("OpenAPI regeneration skipped (unchanged)");
                                }
                            }
                            Ok(Ok(result)) => {
                                let stdout = String::from_utf8_lossy(&result.stdout);
                                let stderr = String::from_utf8_lossy(&result.stderr);
                                warn!(
                                    status = %result.status,
                                    stdout = %stdout,
                                    stderr = %stderr,
                                    "OpenAPI {} failed.",
                                    if is_initial { "generation" } else { "regeneration" }
                                );
                            }
                            Ok(Err(err)) => warn!("Failed to spawn OpenAPI generation: {err}"),
                            Err(_) => warn!("OpenAPI {} timed out.", if is_initial { "generation" } else { "regeneration" }),
                        }
                    }
                }
            }
        }
    });

    Ok(())
}

fn is_python_path(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "py")
}

fn is_ignored_path(path: &PathBuf) -> bool {
    const IGNORED_DIRS: [&str; 11] = [
        ".git",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "venv",
    ];

    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let Some(name) = name.to_str() else {
            return false;
        };
        IGNORED_DIRS.iter().any(|ignored| ignored == &name)
    })
}

fn latest_python_mtime(root: &Path) -> Option<SystemTime> {
    let mut latest = None;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_ignored_path(&entry.path().to_path_buf()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !is_python_path(&path) {
            continue;
        }
        if let Ok(metadata) = fs::metadata(&path) {
            if let Ok(modified) = metadata.modified() {
                if latest.map_or(true, |current| modified > current) {
                    latest = Some(modified);
                }
            }
        }
    }
    latest
}