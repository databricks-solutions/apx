use notify::{RecursiveMode, Watcher};
use std::fs;
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::common::{read_project_metadata, write_metadata_file};
use crate::dev::common::Shutdown;
use crate::dev::watcher::{PollingWatcher, spawn_polling_watcher};
use crate::external::uv::Uv;
use crate::interop::generate_openapi_spec;
use crate::openapi;

/// Generate the OpenAPI spec and TypeScript client for a project.
pub async fn generate_openapi(project_root: &Path) -> Result<(), String> {
    let metadata = read_project_metadata(project_root)?;
    let app_slug = metadata.app_slug.clone();
    let app_entrypoint = metadata.app_entrypoint.clone();

    // Ensure _metadata.py exists before importing the Python module
    write_metadata_file(project_root, &metadata)?;

    let (spec_json, app_slug) =
        generate_openapi_spec(project_root, &app_entrypoint, &app_slug).await?;

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
    fs::write(&api_ts_path, &ts_code).map_err(|err| format!("Failed to write api.ts: {err}"))?;

    debug!(
        api_ts_path = %api_ts_path.display(),
        ts_code_len = ts_code.len(),
        "TypeScript API client generated successfully."
    );

    Ok(())
}

/// Debounce period after a Python file change before regenerating the OpenAPI spec.
/// Prevents rapid-fire regeneration during batch saves.
const OPENAPI_WATCH_DEBOUNCE_MS: u64 = 100;

/// Longer debounce for the initial generation at startup, giving the server time
/// to finish initialization before running `uv run apx __generate_openapi`.
const OPENAPI_INITIAL_DEBOUNCE_MS: u64 = 500;

/// Watches Python files for changes and regenerates the TypeScript API client
/// from the OpenAPI spec.
///
/// Uses both `notify` filesystem events (for responsiveness) and periodic mtime
/// polling (as a fallback) to detect changes. A debounce timer prevents
/// rapid-fire regeneration during batch saves.
struct OpenApiWatcher {
    app_dir: PathBuf,
    uv: Uv,
    /// Receives filesystem events from the `notify` crate. Drained via `try_recv`
    /// on each poll to batch process events.
    notify_rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    /// Kept alive to maintain the recursive filesystem watch.
    _notify_watcher: notify::RecommendedWatcher,
    last_mtime: Option<SystemTime>,
    /// True when a Python change has been detected and generation is pending.
    pending: bool,
    /// Generation fires once this instant passes. Reset on each new change (debounce).
    debounce_until: Option<tokio::time::Instant>,
    /// True until the first generation completes (affects log messages).
    is_initial_generation: bool,
}

impl OpenApiWatcher {
    /// Drain all buffered filesystem events from the notify channel.
    ///
    /// Returns `Break` if the channel is disconnected (notify watcher dropped),
    /// `Continue` otherwise.
    fn drain_notify_events(&mut self) -> ControlFlow<()> {
        loop {
            match self.notify_rx.try_recv() {
                Ok(Ok(event)) => self.process_notify_event(&event),
                Ok(Err(err)) => warn!("OpenAPI watcher error: {err}"),
                Err(mpsc::error::TryRecvError::Empty) => return ControlFlow::Continue(()),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    debug!("OpenAPI watcher channel closed.");
                    return ControlFlow::Break(());
                }
            }
        }
    }

    /// Extract Python file changes from a single notify event, updating mtime
    /// tracking and marking generation as pending.
    fn process_notify_event(&mut self, event: &notify::Event) {
        for path in &event.paths {
            if is_ignored_path(path) || !is_python_path(path) {
                continue;
            }
            self.mark_pending();
            if let Ok(metadata) = fs::metadata(path)
                && let Ok(modified) = metadata.modified()
                && self.last_mtime.is_none_or(|current| modified > current)
            {
                self.last_mtime = Some(modified);
            }
        }
    }

    /// Fallback mtime check for platforms where `notify` is unreliable.
    /// Walks all Python files and compares the latest mtime against the last known.
    fn check_mtime_changes(&mut self) {
        let latest = latest_python_mtime(&self.app_dir);
        let changed =
            latest.is_some_and(|modified| self.last_mtime.is_none_or(|current| modified > current));
        if changed {
            self.mark_pending();
            self.last_mtime = latest;
        }
    }

    /// Record that a Python change was detected and reset the debounce timer.
    fn mark_pending(&mut self) {
        if !self.pending {
            info!("Python change detected, regenerating OpenAPI\u{2026}");
        }
        self.pending = true;
        self.debounce_until =
            Some(tokio::time::Instant::now() + Duration::from_millis(OPENAPI_WATCH_DEBOUNCE_MS));
    }

    /// Run generation if the debounce period has elapsed and a change is pending.
    async fn maybe_generate(&mut self) {
        let Some(until) = self.debounce_until else {
            return;
        };
        if !self.pending || tokio::time::Instant::now() < until {
            return;
        }
        self.debounce_until = None;
        self.pending = false;
        self.run_generation().await;
    }

    /// Spawn `uv run apx __generate_openapi` and log the result.
    async fn run_generation(&mut self) {
        let is_initial = self.is_initial_generation;
        self.is_initial_generation = false;

        if is_initial {
            info!("Running initial OpenAPI generation...");
        } else {
            info!("Running OpenAPI regeneration...");
        }

        let mut cmd = self
            .uv
            .cmd()
            .args(["run", "apx", "__generate_openapi", "--app-dir"])
            .arg(&self.app_dir)
            .cwd(&self.app_dir)
            .into_command();
        let output = tokio::time::timeout(Duration::from_secs(30), cmd.output()).await;
        log_generation_result(output, is_initial);
    }
}

impl PollingWatcher for OpenApiWatcher {
    fn label(&self) -> &'static str {
        "OpenAPI"
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_millis(200)
    }

    async fn poll(&mut self) -> ControlFlow<()> {
        // Drain buffered filesystem events from notify
        if self.drain_notify_events().is_break() {
            return ControlFlow::Break(());
        }
        // Fallback: check mtimes for platforms where notify is unreliable
        self.check_mtime_changes();
        // Run generation if debounce period has elapsed
        self.maybe_generate().await;
        ControlFlow::Continue(())
    }
}

/// Log the result of an OpenAPI generation command.
fn log_generation_result(
    output: Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
    is_initial: bool,
) {
    let label = if is_initial {
        "generation"
    } else {
        "regeneration"
    };
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
                "OpenAPI {label} failed."
            );
        }
        Ok(Err(err)) => warn!("Failed to spawn OpenAPI {label}: {err}"),
        Err(_) => warn!("OpenAPI {label} timed out."),
    }
}

/// Set up and spawn the OpenAPI watcher as a background task.
///
/// Creates a `notify` filesystem watcher, resolves `uv`, and spawns the
/// polling watcher. Returns an error if the filesystem watcher or `uv`
/// resolution fails.
pub async fn start_openapi_watcher(
    app_dir: PathBuf,
    shutdown_rx: broadcast::Receiver<Shutdown>,
) -> Result<(), String> {
    debug!(
        app_dir = %app_dir.display(),
        "Starting OpenAPI watcher."
    );

    let (tx, rx) = mpsc::unbounded_channel();
    let mut notify_watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .map_err(|err| format!("Failed to create file watcher: {err}"))?;
    notify_watcher
        .watch(&app_dir, RecursiveMode::Recursive)
        .map_err(|err| format!("Failed to watch app dir: {err}"))?;

    let uv = Uv::new()
        .await
        .map_err(|e| format!("Failed to resolve uv for OpenAPI watcher: {e}"))?;

    let initial_mtime = latest_python_mtime(&app_dir);
    info!("Will generate OpenAPI on startup");

    let watcher = OpenApiWatcher {
        app_dir,
        uv,
        notify_rx: rx,
        _notify_watcher: notify_watcher,
        last_mtime: initial_mtime,
        pending: true,
        debounce_until: Some(
            tokio::time::Instant::now() + Duration::from_millis(OPENAPI_INITIAL_DEBOUNCE_MS),
        ),
        is_initial_generation: true,
    };

    spawn_polling_watcher(watcher, shutdown_rx);
    Ok(())
}

fn is_python_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "py")
}

fn is_ignored_path(path: &Path) -> bool {
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
        .filter_entry(|entry| !is_ignored_path(entry.path()))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !is_python_path(&path) {
            continue;
        }
        if let Ok(metadata) = fs::metadata(&path)
            && let Ok(modified) = metadata.modified()
            && latest.is_none_or(|current| modified > current)
        {
            latest = Some(modified);
        }
    }
    latest
}
