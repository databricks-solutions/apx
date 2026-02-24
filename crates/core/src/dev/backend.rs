//! Backend (uvicorn) lifecycle manager for the APX dev server.
//!
//! Encapsulates uvicorn spawning, log config resolution, log forwarding,
//! file watching, and environment variable management.
// Runs inside the dev server child process (spawned with Stdio::null()),
// never in the MCP server process — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{debug, info, warn};

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

/// Self-contained backend (uvicorn) lifecycle manager.
/// `ProcessManager` interacts only through this API.
pub(crate) struct Backend {
    child: Arc<Mutex<Option<Child>>>,
    // Immutable config
    app_dir: PathBuf,
    app_slug: String,
    app_entrypoint: String,
    host: String,
    backend_port: u16,
    frontend_port: Option<u16>,
    db_port: u16,
    dev_server_port: u16,
    dev_token: String,
    dev_config: DevConfig,
    // Shared mutable state
    dotenv_vars: Arc<Mutex<HashMap<String, String>>>,
    // DB password (lock-free read after init)
    db: Arc<OnceLock<EmbeddedDb>>,
}

// `Child` does not implement `Debug`, so we provide a manual impl.
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("app_slug", &self.app_slug)
            .field("backend_port", &self.backend_port)
            .finish()
    }
}

impl Backend {
    /// Create a new Backend without spawning the process.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app_dir: PathBuf,
        app_slug: String,
        app_entrypoint: String,
        host: String,
        backend_port: u16,
        frontend_port: Option<u16>,
        db_port: u16,
        dev_server_port: u16,
        dev_token: String,
        dev_config: DevConfig,
        dotenv_vars: Arc<Mutex<HashMap<String, String>>>,
        db: Arc<OnceLock<EmbeddedDb>>,
    ) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            app_dir,
            app_slug,
            app_entrypoint,
            host,
            backend_port,
            frontend_port,
            db_port,
            dev_server_port,
            dev_token,
            dev_config,
            dotenv_vars,
            db,
        }
    }

    pub fn dev_token(&self) -> &str {
        &self.dev_token
    }

    /// Spawn uvicorn. Resolves log config, builds the command, attaches log
    /// forwarders, and stores the child handle.
    pub async fn spawn(&self) -> Result<(), String> {
        let log_config = self.resolve_log_config().await?;
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
            let mut vars = self.dotenv_vars.lock().await;
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

            let mut watcher = match RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    if let Ok(event) = res {
                        let _ = tx.blocking_send(event);
                    }
                },
                notify::Config::default(),
            ) {
                Ok(w) => w,
                Err(e) => {
                    warn!("Failed to create file watcher: {}", e);
                    return;
                }
            };

            let watched_files = vec![
                backend.app_dir.join(".env"),
                backend.app_dir.join("pyproject.toml"),
                backend.app_dir.join("uv.lock"),
            ];

            for file in &watched_files {
                if file.exists()
                    && let Err(e) = watcher.watch(file, RecursiveMode::NonRecursive)
                {
                    warn!("Failed to watch file {:?}: {}", file, e);
                }
            }

            let debounce_duration = Duration::from_millis(150);

            while let Some(event) = rx.recv().await {
                if !matches!(
                    event.kind,
                    notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                ) {
                    continue;
                }

                let mut triggered_file = None;
                for path in &event.paths {
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    if ["pyproject.toml", "uv.lock", ".env"].contains(&file_name) {
                        triggered_file = Some(file_name.to_string());
                        break;
                    }
                }

                if let Some(mut file_name) = triggered_file {
                    // Debounce: wait for more events
                    tokio::time::sleep(debounce_duration).await;

                    // Drain additional events during the debounce period
                    let mut received_more = false;
                    while let Ok(additional_event) = rx.try_recv() {
                        received_more = true;
                        for path in &additional_event.paths {
                            let additional_file_name =
                                path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            if ["pyproject.toml", "uv.lock", ".env"].contains(&additional_file_name)
                            {
                                file_name = additional_file_name.to_string();
                            }
                        }
                    }

                    // If we received more events, continue the loop to debounce again
                    if received_more {
                        continue;
                    }

                    // Guard against concurrent restarts
                    if restarting
                        .compare_exchange(
                            false,
                            true,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        )
                        .is_err()
                    {
                        debug!("Restart already in progress, skipping.");
                        continue;
                    }

                    info!("{} changed, restarting uvicorn", file_name);

                    // Run uv sync if Python dependencies changed
                    let needs_sync = file_name == "pyproject.toml" || file_name == "uv.lock";
                    if needs_sync {
                        info!("Running uv sync due to {} change", file_name);
                        if let Err(e) = crate::common::uv_sync(&backend.app_dir).await {
                            warn!("uv sync failed: {}", e);
                        }
                    }

                    // Reload .env if it exists
                    let new_vars =
                        if let Ok(dotenv) = DotenvFile::read(&backend.app_dir.join(".env")) {
                            dotenv.get_vars()
                        } else {
                            HashMap::new()
                        };

                    // Stop → update vars → respawn
                    backend.stop_current().await;
                    {
                        let mut vars = backend.dotenv_vars.lock().await;
                        *vars = new_vars;
                    }
                    if let Err(e) = backend.spawn().await {
                        warn!("Failed to restart backend: {}", e);
                    }

                    restarting.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
    }

    // -- private helpers --

    /// Resolve uvicorn logging config, with validation and fallback.
    async fn resolve_log_config(&self) -> Result<String, String> {
        let log_config_result =
            resolve_log_config(&self.dev_config, &self.app_slug, &self.app_dir).await?;

        match &log_config_result {
            LogConfigResult::PythonFile(path) => Ok(path.display().to_string()),
            LogConfigResult::JsonConfig(config_path) => {
                // Validate the JSON config can be loaded by Python's logging.config.dictConfig
                let validation_script = format!(
                    "import json, logging.config; logging.config.dictConfig(json.load(open('{}')))",
                    config_path.display()
                );

                let output = UvTool::new("python")
                    .await?
                    .cmd()
                    .args(["-c", &validation_script])
                    .cwd(&self.app_dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .exec()
                    .await
                    .map_err(|e| format!("Failed to validate logging config: {e}"))?;

                if output.exit_code == Some(0) {
                    Ok(config_path.display().to_string())
                } else {
                    // Validation failed — fall back to default config
                    let stderr = &output.stderr;
                    warn!(
                        "Logging config validation failed, falling back to default:\n{}",
                        stderr
                    );
                    eprintln!(
                        "⚠️  Custom logging config is invalid, using default config:\n{}",
                        stderr
                    );

                    let default_config = default_logging_config(&self.app_slug);
                    let fallback_path = write_logging_config_json(&default_config, &self.app_dir)
                        .await
                        .map_err(|e| format!("Failed to write fallback logging config: {e}"))?;
                    Ok(fallback_path.display().to_string())
                }
            }
        }
    }

    /// Construct the uvicorn `ToolCommand` with all env vars.
    async fn build_uvicorn_command(
        &self,
        log_config: &str,
    ) -> Result<crate::external::ToolCommand, String> {
        let mut tool_cmd = UvTool::new("uvicorn")
            .await?
            .cmd()
            .args([
                &self.app_entrypoint,
                "--host",
                &self.host,
                "--port",
                &self.backend_port.to_string(),
                "--reload",
                "--log-config",
                log_config,
            ])
            .cwd(&self.app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("APX_BACKEND_PORT", self.backend_port.to_string())
            .env("APX_DEV_DB_PORT", self.db_port.to_string())
            .env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", &self.host)
            .env(token::DEV_TOKEN_ENV, &self.dev_token)
            // Databricks SDK user-agent tracking via env vars
            .env("DATABRICKS_SDK_UPSTREAM", "apx")
            .env("DATABRICKS_SDK_UPSTREAM_VERSION", apx_common::VERSION)
            // Force Python to flush stdout/stderr immediately (no buffering)
            .env("PYTHONUNBUFFERED", "1");

        if let Some(fp) = self.frontend_port {
            tool_cmd = tool_cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }

        if let Some(db) = self.db.get() {
            tool_cmd = tool_cmd.env("APX_DEV_DB_PWD", db.password());
        }

        // Apply dotenv variables
        let vars = self.dotenv_vars.lock().await;
        for (key, value) in vars.iter() {
            tool_cmd = tool_cmd.env(key, value);
        }

        Ok(tool_cmd)
    }

    /// Spawn tasks to read stdout/stderr, prefix with source, and forward to flux.
    fn attach_log_forwarders(&self, child: &mut Child) {
        let service_name = format!("{}_app", self.app_slug);
        let app_path = self.app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let service_name = service_name.clone();
            let app_path = app_path.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!(
                        "{}",
                        apx_common::format::format_process_log_line("app", &line)
                    );
                    forward_log_to_flux(&line, "INFO", &service_name, &app_path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
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

    /// Stop the current backend process tree.
    async fn stop_current(&self) {
        stop_child_tree(self.label(), &self.child).await;
    }
}

impl DevProcess for Backend {
    fn child_handle(&self) -> &Arc<Mutex<Option<Child>>> {
        &self.child
    }

    fn label(&self) -> &'static str {
        "backend"
    }

    async fn status(&self) -> String {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            None => return "stopped".to_string(),
            Some(process) => match process.try_wait() {
                Ok(None) => {} // still running — continue to HTTP probe
                Ok(Some(_)) => return "failed".to_string(),
                Err(_) => return "error".to_string(),
            },
        }
        drop(guard);

        // Process is running — do HTTP health probe
        match http_health_probe(CLIENT_HOST, self.backend_port).await {
            ProbeResult::Responded(_) => "healthy".to_string(),
            ProbeResult::Failed(_) => "starting".to_string(),
        }
    }
}
