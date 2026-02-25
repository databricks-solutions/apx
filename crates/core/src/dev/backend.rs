//! Backend (uvicorn) lifecycle manager for the APX dev server.
//!
//! Encapsulates uvicorn spawning, log config resolution, log forwarding,
//! file watching, and environment variable management.
// Runs inside the dev server process (in-process for attached mode,
// child process for detached mode). Never in the MCP server process
// — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{info, warn};

use crate::dev::common::{DevProcess, ProbeResult, http_health_probe, stop_child_tree};
use crate::dev::embedded_db::EmbeddedDb;
use crate::dev::otel::forward_log_to_flux;
use crate::dev::token;
use crate::dotenv::DotenvFile;
use crate::external::uv::UvTool;
use crate::python_logging::{
    DevConfig, LogConfigResult, default_logging_config, resolve_log_config,
    write_logging_config_json,
};
use apx_common::hosts::CLIENT_HOST;

/// Files that trigger a backend restart when modified.
const WATCHED_FILES: &[&str] = &[".env", "pyproject.toml", "uv.lock"];

/// Files that require `uv sync` before restarting.
const DEPENDENCY_FILES: &[&str] = &["pyproject.toml", "uv.lock"];

/// Debounce window for file change events (ms).
const DEBOUNCE_MS: u64 = 150;

// ---------------------------------------------------------------------------
// BackendConfig — named constructor parameters
// ---------------------------------------------------------------------------

/// All immutable and shared-state values needed to construct a [`Backend`].
/// Avoids a 12-parameter positional constructor.
pub struct BackendConfig {
    pub app_dir: PathBuf,
    pub app_slug: String,
    pub app_entrypoint: String,
    pub host: String,
    pub backend_port: u16,
    pub frontend_port: Option<u16>,
    pub db_port: u16,
    pub dev_server_port: u16,
    pub dev_token: String,
    pub dev_config: DevConfig,
    pub dotenv_vars: Arc<Mutex<HashMap<String, String>>>,
    pub db: Arc<OnceLock<EmbeddedDb>>,
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Self-contained backend (uvicorn) lifecycle manager.
/// `ProcessManager` interacts only through this API.
pub struct Backend {
    child: Arc<Mutex<Option<Child>>>,
    cfg: BackendConfig,
}

// `Child` does not implement `Debug`, so we provide a manual impl.
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("app_slug", &self.cfg.app_slug)
            .field("backend_port", &self.cfg.backend_port)
            .finish()
    }
}

impl Backend {
    pub fn new(cfg: BackendConfig) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cfg,
        }
    }

    pub fn dev_token(&self) -> &str {
        &self.cfg.dev_token
    }

    /// Spawn uvicorn. Resolves log config, builds the command, attaches log
    /// forwarders, and stores the child handle.
    pub async fn spawn(&self) -> Result<(), String> {
        let log_config = self.resolve_and_validate_log_config().await?;
        let tool_cmd = self.build_uvicorn_command(&log_config).await?;

        let mut child = tool_cmd.spawn().map_err(String::from)?;
        self.attach_log_forwarders(&mut child);

        let mut guard = self.child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    /// Stop the current backend, update env vars, and respawn.
    pub async fn restart_with_env(&self, new_vars: HashMap<String, String>) -> Result<(), String> {
        self.stop_current().await;
        {
            let mut vars = self.cfg.dotenv_vars.lock().await;
            *vars = new_vars;
        }
        self.spawn().await
    }

    /// Watch `.env`, `pyproject.toml`, and `uv.lock` for changes and restart
    /// uvicorn when any of them change.
    pub fn start_file_watcher(self: &Arc<Self>) {
        let backend = Arc::clone(self);
        let restarting = Arc::new(std::sync::atomic::AtomicBool::new(false));

        tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(100);

            let Some(mut watcher) = create_watcher(tx) else {
                return;
            };
            register_watches(&mut watcher, &backend.cfg.app_dir);

            while let Some(event) = rx.recv().await {
                let Some(file_name) = classify_event(&event) else {
                    continue;
                };
                let Some(file_name) = debounce(&mut rx, file_name).await else {
                    continue;
                };
                if !try_acquire_restart(&restarting) {
                    continue;
                }

                handle_file_change(&backend, &file_name).await;

                restarting.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        });
    }

    // -- private: log config --

    /// Resolve uvicorn logging config. For JSON configs, validates via Python
    /// and falls back to the default config on failure.
    async fn resolve_and_validate_log_config(&self) -> Result<String, String> {
        let result =
            resolve_log_config(&self.cfg.dev_config, &self.cfg.app_slug, &self.cfg.app_dir).await?;

        match &result {
            LogConfigResult::PythonFile(path) => Ok(path.display().to_string()),
            LogConfigResult::JsonConfig(path) => self.validate_json_log_config(path).await,
        }
    }

    /// Run Python's `logging.config.dictConfig` against a JSON config file.
    /// Returns the config path on success, or generates a default fallback.
    async fn validate_json_log_config(
        &self,
        config_path: &std::path::Path,
    ) -> Result<String, String> {
        const VALIDATE_LOGGING_CONFIG: &str = "import sys, json, logging.config; logging.config.dictConfig(json.load(open(sys.argv[1])))";

        let output = UvTool::new("python")
            .await?
            .cmd()
            .args([
                "-c",
                VALIDATE_LOGGING_CONFIG,
                &config_path.display().to_string(),
            ])
            .cwd(&self.cfg.app_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .exec()
            .await
            .map_err(|e| format!("Failed to validate logging config: {e}"))?;

        if output.exit_code == Some(0) {
            return Ok(config_path.display().to_string());
        }

        warn!(
            "Logging config validation failed, falling back to default:\n{}",
            output.stderr
        );
        eprintln!(
            "⚠️  Custom logging config is invalid, using default config:\n{}",
            output.stderr
        );

        let default = default_logging_config(&self.cfg.app_slug);
        let path = write_logging_config_json(&default, &self.cfg.app_dir)
            .await
            .map_err(|e| format!("Failed to write fallback logging config: {e}"))?;
        Ok(path.display().to_string())
    }

    // -- private: command construction --

    /// Construct the `uv run uvicorn` command with all env vars.
    async fn build_uvicorn_command(
        &self,
        log_config: &str,
    ) -> Result<crate::external::ToolCommand, String> {
        let cfg = &self.cfg;

        let mut cmd = UvTool::new("uvicorn")
            .await?
            .cmd()
            .args([
                &cfg.app_entrypoint,
                "--host",
                &cfg.host,
                "--port",
                &cfg.backend_port.to_string(),
                "--reload",
                "--log-config",
                log_config,
            ])
            .cwd(&cfg.app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // APX runtime context
            .env("APX_BACKEND_PORT", cfg.backend_port.to_string())
            .env("APX_DEV_DB_PORT", cfg.db_port.to_string())
            .env("APX_DEV_SERVER_PORT", cfg.dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", &cfg.host)
            .env(token::DEV_TOKEN_ENV, &cfg.dev_token)
            // Databricks SDK user-agent tracking
            .env("DATABRICKS_SDK_UPSTREAM", "apx")
            .env("DATABRICKS_SDK_UPSTREAM_VERSION", apx_common::VERSION)
            // Force Python to flush stdout/stderr immediately
            .env("PYTHONUNBUFFERED", "1");

        if let Some(fp) = cfg.frontend_port {
            cmd = cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }
        if let Some(db) = cfg.db.get() {
            cmd = cmd.env("APX_DEV_DB_PWD", db.password());
        } else {
            warn!("No database found for backend, APX_DEV_DB_PWD will not be set");
        }

        let vars = cfg.dotenv_vars.lock().await;
        for (key, value) in vars.iter() {
            cmd = cmd.env(key, value);
        }

        Ok(cmd)
    }

    // -- private: log forwarding --

    /// Spawn tasks to read stdout/stderr, prefix with source, and forward to flux.
    fn attach_log_forwarders(&self, child: &mut Child) {
        let service_name = format!("{}_app", self.cfg.app_slug);
        let app_path = self.cfg.app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let svc = service_name.clone();
            let path = app_path.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!(
                        "{}",
                        apx_common::format::format_process_log_line("app", &line)
                    );
                    forward_log_to_flux(&line, "INFO", &svc, &path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!(
                        "{}",
                        apx_common::format::format_process_log_line("app", &line)
                    );
                    let severity = apx_common::format::parse_python_severity(&line);
                    forward_log_to_flux(&line, severity, &service_name, &app_path).await;
                }
            });
        }
    }

    // -- private: process control --

    /// Stop the current backend process tree.
    async fn stop_current(&self) {
        stop_child_tree(self.label(), &self.child).await;
    }
}

// ---------------------------------------------------------------------------
// DevProcess impl
// ---------------------------------------------------------------------------

impl DevProcess for Backend {
    fn child_handle(&self) -> &Arc<Mutex<Option<Child>>> {
        &self.child
    }

    fn label(&self) -> &'static str {
        "backend"
    }

    async fn status(&self) -> &'static str {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            None => return "stopped",
            Some(process) => match process.try_wait() {
                Ok(None) => {} // still running — continue to HTTP probe
                Ok(Some(_)) => return "failed",
                Err(_) => return "error",
            },
        }
        drop(guard);

        match http_health_probe(CLIENT_HOST, self.cfg.backend_port).await {
            ProbeResult::Responded => "healthy",
            ProbeResult::Failed => "starting",
        }
    }
}

// ---------------------------------------------------------------------------
// File watcher helpers — free functions to keep start_file_watcher short
// ---------------------------------------------------------------------------

/// Create a `notify` watcher that sends events to `tx`.
fn create_watcher(tx: tokio::sync::mpsc::Sender<Event>) -> Option<RecommendedWatcher> {
    match RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => Some(w),
        Err(e) => {
            warn!("Failed to create file watcher: {}", e);
            None
        }
    }
}

/// Register watches on known project files (only if they exist on disk).
fn register_watches(watcher: &mut RecommendedWatcher, app_dir: &std::path::Path) {
    for name in WATCHED_FILES {
        let path = app_dir.join(name);
        if path.exists()
            && let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive)
        {
            warn!("Failed to watch file {:?}: {}", path, e);
        }
    }
}

/// If the event is a create/modify on a watched file, return its file name.
fn classify_event(event: &Event) -> Option<String> {
    if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
        return None;
    }

    event.paths.iter().find_map(|path| {
        let name = path.file_name()?.to_str()?;
        WATCHED_FILES.contains(&name).then(|| name.to_string())
    })
}

/// Wait for the debounce window, drain queued events, and return the latest
/// triggered file name. Returns `None` if more events arrived (caller should
/// re-enter the loop to debounce again).
async fn debounce(
    rx: &mut tokio::sync::mpsc::Receiver<Event>,
    mut file_name: String,
) -> Option<String> {
    tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;

    let mut received_more = false;
    while let Ok(extra) = rx.try_recv() {
        received_more = true;
        if let Some(name) = classify_event(&extra) {
            file_name = name;
        }
    }

    if received_more { None } else { Some(file_name) }
}

/// Atomically try to set the `restarting` flag. Returns `true` if acquired.
fn try_acquire_restart(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.compare_exchange(
        false,
        true,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
    )
    .is_ok()
}

/// Execute a single file-change restart cycle: sync deps, reload env, respawn.
async fn handle_file_change(backend: &Backend, file_name: &str) {
    info!("{} changed, restarting uvicorn", file_name);

    if DEPENDENCY_FILES.contains(&file_name) {
        info!("Running uv sync due to {} change", file_name);
        if let Err(e) = crate::common::uv_sync(&backend.cfg.app_dir).await {
            warn!("uv sync failed: {}", e);
        }
    }

    let new_vars = DotenvFile::read(&backend.cfg.app_dir.join(".env"))
        .map(|d| d.get_vars())
        .unwrap_or_default();

    backend.stop_current().await;
    {
        let mut vars = backend.cfg.dotenv_vars.lock().await;
        *vars = new_vars;
    }
    if let Err(e) = backend.spawn().await {
        warn!("Failed to restart backend: {}", e);
    }
}
