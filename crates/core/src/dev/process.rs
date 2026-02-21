//! Process management for APX dev server.
//!
//! Manages frontend (Vite/Bun), backend (uvicorn), and database (PGlite) processes.
//! Subprocess stdout/stderr are captured and forwarded to flux for centralized logging.
// Runs inside the dev server child process (spawned with Stdio::null()),
// never in the MCP server process — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rand::{Rng, distributions::Alphanumeric};
use sysinfo::{Pid, Signal, System};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};

/// Server-side probe timeout in seconds.
/// Must be strictly less than the client-side per-request timeout (DEFAULT_TIMEOUT_SECS in client.rs)
/// to avoid a race where both timeouts fire simultaneously, causing every poll cycle to fail.
const PROBE_TIMEOUT_SECS: u64 = 1;

/// Shared HTTP client for health probes.
/// Reused across all health checks to avoid creating a new client per probe.
static HEALTH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(2)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Result of an HTTP health probe against a backend/frontend service.
/// Fields are used for logging inside `http_health_probe`.
#[allow(dead_code)]
enum ProbeResult {
    /// Service responded with the given HTTP status code — it is up.
    Responded(u16),
    /// Connection or timeout error — service is not ready yet.
    Failed(String),
}

use crate::common::read_project_metadata;
use crate::dev::otel::forward_log_to_flux;
use crate::dotenv::DotenvFile;
use crate::external::ExternalTool;
use crate::external::bun::Bun;
use crate::external::uv::{ApxTool, UvTool};
use crate::python_logging::{
    DevConfig, LogConfigResult, default_logging_config, resolve_log_config,
    write_logging_config_json,
};
use apx_common::hosts::CLIENT_HOST;

#[derive(Debug, Clone, Copy)]
enum LogSource {
    App,
    Db,
}

impl LogSource {
    fn as_str(&self) -> &'static str {
        match self {
            LogSource::App => "app",
            LogSource::Db => "db",
        }
    }
}

impl fmt::Display for LogSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Format a log line with local timestamp and source prefix.
/// Delegates to the centralized formatter in `apx_common::format`.
fn format_log_line(source: LogSource, message: &str) -> String {
    apx_common::format::format_process_log_line(source.as_str(), message)
}

/// Setup sitecustomize.py for Databricks SDK user-agent tracking.
/// This is non-critical telemetry - failures are silently ignored.
/// Returns the temp directory path if successful, None otherwise.
fn setup_sitecustomize() -> Option<PathBuf> {
    let temp_dir = tempfile::tempdir().ok()?;
    let sitecustomize_path = temp_dir.path().join("sitecustomize.py");
    std::fs::write(
        &sitecustomize_path,
        r#"import os
try:
    from databricks.sdk.core import with_user_agent_extra, with_product
    from apx import __version__
    if os.getenv('APX_UVICORN') == '1':
        with_user_agent_extra('apx', __version__)
        with_product('apx', __version__)
except Exception:
    pass
"#,
    )
    .ok()?;
    // Use into_path() to prevent automatic cleanup - dir persists for uvicorn's lifetime
    Some(temp_dir.keep())
}

#[derive(Debug)]
pub struct ProcessManager {
    frontend_child: Arc<Mutex<Option<Child>>>,
    backend_child: Arc<Mutex<Option<Child>>>,
    db_child: Arc<Mutex<Option<Child>>>,
    backend_port: u16,
    frontend_port: Option<u16>,
    db_port: u16,
    dev_server_port: u16,
    host: String,
    dev_token: String,
    db_password: String,
    app_dir: PathBuf,
    app_slug: String,
    app_entrypoint: String,
    dotenv_vars: Arc<Mutex<HashMap<String, String>>>,
    dev_config: DevConfig,
    has_ui: bool,
}

impl ProcessManager {
    /// Create a new ProcessManager without spawning processes.
    /// Call `start_processes()` to spawn processes in the background.
    pub fn new(
        app_dir: &Path,
        host: &str,
        dev_server_port: u16,
        backend_port: u16,
        frontend_port: Option<u16>,
        db_port: u16,
    ) -> Result<Self, String> {
        // Note: Preflight checks (metadata, uv sync, bun install) are done client-side in start.rs
        let metadata = read_project_metadata(app_dir)?;
        let has_ui = metadata.has_ui();

        let dotenv = DotenvFile::read(&app_dir.join(".env"))?;
        let dotenv_vars = Arc::new(Mutex::new(dotenv.get_vars()));
        let app_slug = metadata.app_slug.clone();
        let app_entrypoint = metadata.app_entrypoint.clone();
        let dev_config = metadata.dev_config.clone();

        let dev_token = Self::generate_dev_token();
        let db_password = Self::generate_dev_token(); // Random password for PGlite

        debug!(
            app_dir = %app_dir.display(),
            host = %host,
            dev_server_port,
            backend_port,
            ?frontend_port,
            db_port,
            has_ui,
            "Creating ProcessManager"
        );

        Ok(Self {
            frontend_child: Arc::new(Mutex::new(None)),
            backend_child: Arc::new(Mutex::new(None)),
            db_child: Arc::new(Mutex::new(None)),
            backend_port,
            frontend_port,
            db_port,
            dev_server_port,
            host: host.to_string(),
            dev_token,
            db_password,
            app_dir: app_dir
                .canonicalize()
                .unwrap_or_else(|_| app_dir.to_path_buf()),
            app_slug,
            app_entrypoint,
            dotenv_vars,
            dev_config,
            has_ui,
        })
    }

    /// Spawn processes in background (DB → Vite → Uvicorn).
    /// DB is non-critical - failures are logged but don't block other processes.
    /// This method spawns a background task and returns immediately.
    pub fn start_processes(self: &Arc<Self>) {
        let pm = Arc::clone(self);
        tokio::spawn(async move {
            // 1. DB (non-critical) - warn on failure but continue
            debug!("Starting PGlite database process...");
            match Self::ensure_bun().await {
                Ok(bun) => {
                    if let Err(e) = pm.spawn_pglite(&bun).await {
                        warn!(
                            "⚠️ Failed to start PGlite database: {}. Continuing without DB.",
                            e
                        );
                        // Don't return - continue with other processes
                    } else {
                        debug!("PGlite database started successfully");
                    }
                }
                Err(e) => {
                    warn!(
                        "⚠️ Bun not available for PGlite: {}. Continuing without DB.",
                        e
                    );
                }
            }

            // 2. Vite (critical, but only if project has UI)
            if pm.has_ui {
                debug!("Starting Vite frontend process...");
                if let Err(e) = pm.spawn_bun_dev(&pm.app_dir).await {
                    warn!("Failed to start frontend: {}", e);
                    return; // Critical failure
                }
                debug!("Vite frontend started successfully");
            } else {
                debug!("Skipping Vite frontend (backend-only project)");
            }

            // 3. Uvicorn (critical)
            debug!("Starting uvicorn backend process...");
            if let Err(e) = pm
                .spawn_uvicorn(&pm.app_dir, pm.app_entrypoint.clone())
                .await
            {
                warn!("Failed to start backend: {}", e);
                return; // Critical failure
            }
            debug!("Uvicorn backend started successfully");

            debug!("All processes spawned, starting file watcher");
            pm.start_backend_file_watcher();
        });
    }

    pub fn dev_token(&self) -> &str {
        &self.dev_token
    }

    #[allow(dead_code)]
    pub fn app_dir(&self) -> &Path {
        &self.app_dir
    }

    /// Stop all managed processes using a phased shutdown approach:
    /// 1. Send SIGTERM to allow graceful shutdown
    /// 2. Wait briefly for processes to exit
    /// 3. Force kill any remaining processes
    pub async fn stop(&self) {
        debug!(
            host = %self.host,
            frontend_port = ?self.frontend_port,
            backend_port = self.backend_port,
            db_port = self.db_port,
            dev_server_port = self.dev_server_port,
            "Stopping dev processes with phased shutdown."
        );

        // Phase 1: Send SIGTERM to all processes (polite request to stop)
        debug!("Phase 1: Sending SIGTERM to all processes.");
        Self::send_sigterm("backend", &self.backend_child).await;
        Self::send_sigterm("frontend", &self.frontend_child).await;
        Self::send_sigterm("db", &self.db_child).await;

        // Phase 2: Wait briefly for graceful exit (500ms)
        debug!("Phase 2: Waiting for graceful exit.");
        let wait_backend = Self::wait_for_child("backend", &self.backend_child);
        let wait_frontend = Self::wait_for_child("frontend", &self.frontend_child);
        let wait_db = Self::wait_for_child("db", &self.db_child);
        let _ = timeout(Duration::from_millis(500), async {
            tokio::join!(wait_backend, wait_frontend, wait_db)
        })
        .await;

        // Phase 3: Force kill any remaining processes
        debug!("Phase 3: Force killing remaining processes.");
        Self::force_kill("backend", &self.backend_child).await;
        Self::force_kill("frontend", &self.frontend_child).await;
        Self::force_kill("db", &self.db_child).await;

        debug!("All processes stopped.");
    }

    /// Get the status of all managed processes.
    /// Runs all three checks in parallel using tokio::join! to avoid blocking.
    pub async fn status(&self) -> (String, String, String) {
        // Run all checks in parallel - no mutex held during HTTP probes
        // Use CLIENT_HOST (127.0.0.1) for health probes, not the bind host (0.0.0.0)
        // which doesn't resolve to localhost on Windows.
        let frontend_http_check = self.frontend_port.map(|p| (CLIENT_HOST, p));
        let (frontend_status, backend_status, db_status) = tokio::join!(
            self.status_for_process(&self.frontend_child, frontend_http_check),
            self.status_for_process(&self.backend_child, Some((CLIENT_HOST, self.backend_port))),
            self.status_for_process(&self.db_child, None), // DB: no HTTP check, just process status
        );
        // For backend-only projects, report frontend as "n/a" instead of "stopped"
        let frontend_status = if !self.has_ui && frontend_status == "stopped" {
            "n/a".to_string()
        } else {
            frontend_status
        };
        (frontend_status, backend_status, db_status)
    }

    /// Returns true if this project has a frontend (UI).
    pub fn has_ui(&self) -> bool {
        self.has_ui
    }

    pub async fn restart_uvicorn_with_env(
        &self,
        new_vars: HashMap<String, String>,
    ) -> Result<(), String> {
        Self::stop_child_tree("backend", &self.backend_child).await;
        {
            let mut vars = self.dotenv_vars.lock().await;
            *vars = new_vars;
        }
        self.spawn_uvicorn(&self.app_dir, self.app_entrypoint.clone())
            .await
    }

    async fn spawn_bun_dev(&self, app_dir: &Path) -> Result<(), String> {
        // ============================================================================
        // IMPORTANT: Frontend logs are NOT piped through apx stdout/stderr.
        // The frontend process sends logs directly to flux via OTEL SDK.
        // This ensures proper service attribution (service.name = {app}_ui) and avoids
        // log interleaving issues that occur when multiple processes share stdout.
        // See entrypoint.ts for OTEL initialization.
        // ============================================================================

        // Use ApxTool to invoke `apx frontend dev` via uv
        let mut tool_cmd = ApxTool::new_apx()
            .await?
            .cmd()
            .args(["frontend", "dev"])
            .cwd(app_dir)
            .stdin(Stdio::null())
            // Inherit stdout/stderr for local visibility, but don't capture/forward
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env("APX_BACKEND_PORT", self.backend_port.to_string())
            .env("APX_DEV_DB_PORT", self.db_port.to_string())
            .env("APX_DEV_DB_PWD", &self.db_password)
            .env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", &self.host)
            .env("APX_DEV_TOKEN", &self.dev_token)
            .env("APX_APP_NAME", &self.app_slug)
            .env("APX_APP_PATH", self.app_dir.display().to_string())
            // OpenTelemetry configuration - frontend sends logs directly to flux
            .env(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                format!("http://{}:{}", CLIENT_HOST, crate::flux::FLUX_PORT),
            )
            .env(apx_common::hosts::ENV_FRONTEND_HOST, CLIENT_HOST)
            .env("OTEL_SERVICE_NAME", format!("{}_ui", self.app_slug));

        if let Some(fp) = self.frontend_port {
            tool_cmd = tool_cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }

        let child = tool_cmd.spawn().map_err(String::from)?;

        let mut guard = self.frontend_child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn spawn_uvicorn(&self, app_dir: &Path, app_entrypoint: String) -> Result<(), String> {
        // ============================================================================
        // Backend logs are captured via stdout/stderr and forwarded to flux.
        // No OTEL Python dependencies required - apx handles log collection.
        // Log lines are prefixed with timestamp and source for visibility:
        //   2026-01-28 14:09:02.413 |  app | INFO: Uvicorn running...
        // ============================================================================

        // Resolve uvicorn logging config (inline TOML, external Python file, or default)
        let log_config_result =
            resolve_log_config(&self.dev_config, &self.app_slug, app_dir).await?;
        let log_config = log_config_result.to_string_path();

        // Setup sitecustomize.py for user-agent tracking (non-critical, best-effort)
        let sitecustomize_dir = setup_sitecustomize();

        // Run uvicorn via uv to ensure correct Python environment
        let mut tool_cmd = UvTool::new("uvicorn")
            .await?
            .cmd()
            .args([
                &app_entrypoint,
                "--host",
                &self.host,
                "--port",
                &self.backend_port.to_string(),
                "--reload",
                "--log-config",
                &log_config,
            ])
            .cwd(app_dir)
            .stdin(Stdio::null())
            // Capture stdout/stderr for prefixed logging and flux forwarding
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("APX_BACKEND_PORT", self.backend_port.to_string())
            .env("APX_DEV_DB_PORT", self.db_port.to_string())
            .env("APX_DEV_DB_PWD", &self.db_password)
            .env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", &self.host)
            .env("APX_DEV_TOKEN", &self.dev_token)
            .env("APX_UVICORN", "1")
            // Force Python to flush stdout/stderr immediately (no buffering)
            .env("PYTHONUNBUFFERED", "1");

        if let Some(fp) = self.frontend_port {
            tool_cmd = tool_cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }

        // Prepend sitecustomize dir to PYTHONPATH if setup succeeded (non-critical)
        if let Some(sitecustomize_path) = sitecustomize_dir {
            let pythonpath = match std::env::var("PYTHONPATH") {
                Ok(existing) => format!("{}:{}", sitecustomize_path.display(), existing),
                Err(_) => sitecustomize_path.display().to_string(),
            };
            tool_cmd = tool_cmd.env("PYTHONPATH", pythonpath);
        }

        // Apply dotenv variables
        let vars = self.dotenv_vars.lock().await;
        for (key, value) in vars.iter() {
            tool_cmd = tool_cmd.env(key, value);
        }
        drop(vars);

        let mut child = tool_cmd.spawn().map_err(String::from)?;

        // Spawn tasks to read stdout/stderr, prefix with source, and forward to flux
        let service_name = format!("{}_app", self.app_slug);
        let app_path = self.app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let service_name = service_name.clone();
            let app_path = app_path.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{}", format_log_line(LogSource::App, &line));
                    forward_log_to_flux(&line, "INFO", &service_name, &app_path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{}", format_log_line(LogSource::App, &line));
                    let severity = apx_common::format::parse_python_severity(&line);
                    forward_log_to_flux(&line, severity, &service_name, &app_path).await;
                }
            });
        }

        let mut guard = self.backend_child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn spawn_pglite(&self, bun: &Bun) -> Result<(), String> {
        let child = self
            .spawn_process(
                &self.app_dir,
                bun.binary_path().to_path_buf(),
                vec![
                    "x".to_string(),
                    "@electric-sql/pglite-socket".to_string(),
                    "--db=memory://".to_string(),
                    format!("--host={}", self.host),
                    "--debug=0".to_string(),
                    format!("--port={}", self.db_port),
                ],
                LogSource::Db,
                false,
            )
            .await?;

        let mut guard = self.db_child.lock().await;
        *guard = Some(child);

        // Wait for PGlite to be ready and change the default password
        // Use CLIENT_HOST (127.0.0.1) for connections, not the bind host (0.0.0.0)
        Self::wait_for_db_ready(CLIENT_HOST, self.db_port).await?;
        Self::change_db_password(CLIENT_HOST, self.db_port, &self.db_password).await?;
        debug!("PGlite password changed successfully");

        self.spawn_db_health_monitor();
        Ok(())
    }

    /// Wait for PGlite database to be ready to accept connections.
    async fn wait_for_db_ready(host: &str, port: u16) -> Result<(), String> {
        for _ in 0..30 {
            if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!("PGlite not ready on {host}:{port}"))
    }

    /// Change the PGlite database password using tokio-postgres.
    /// Important: PGlite only supports one connection at a time, so we must
    /// ensure the connection is fully closed before returning.
    async fn change_db_password(host: &str, port: u16, new_password: &str) -> Result<(), String> {
        use tokio_postgres::NoTls;

        let conn_str =
            format!("host={host} port={port} user=postgres password=postgres dbname=postgres");

        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .map_err(|e| format!("Failed to connect to PGlite: {e}"))?;

        // Spawn connection task with a handle so we can wait for it
        let conn_handle = tokio::spawn(async move {
            if let Err(e) = connection.await {
                warn!("PGlite connection error: {}", e);
            }
        });

        // Escape single quotes for SQL safety
        let escaped = new_password.replace('\'', "''");
        let query = format!("ALTER USER postgres WITH PASSWORD '{escaped}'");

        let result = client
            .execute(&query, &[])
            .await
            .map_err(|e| format!("Failed to change password: {e}"));

        // Drop the client to signal the connection to close
        drop(client);

        // Wait up to 5 seconds for the connection task to exit
        match timeout(Duration::from_secs(5), conn_handle).await {
            Ok(Ok(())) => {
                // connection task exited cleanly
            }
            Ok(Err(e)) => {
                warn!("Postgres connection task panicked: {}", e);
            }
            Err(_) => {
                warn!("Timed out waiting for Postgres connection to shut down");
            }
        }

        result.map(|_| ())
    }

    fn spawn_db_health_monitor(&self) {
        let db_child = Arc::clone(&self.db_child);
        tokio::spawn(async move {
            let start_time = chrono::Utc::now();
            let timeout_duration = chrono::Duration::seconds(60);

            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let elapsed = chrono::Utc::now() - start_time;

                if elapsed > timeout_duration {
                    break;
                }

                let mut guard = db_child.lock().await;
                if let Some(child) = guard.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            warn!("PGLite process exited early with status: {:?}", status);
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            warn!("Failed to check PGLite process status: {}", e);
                            break;
                        }
                    }
                } else {
                    warn!("PGLite process handle lost");
                    break;
                }
            }
        });
    }

    fn start_backend_file_watcher(&self) {
        let app_dir = self.app_dir.clone();
        let dotenv_vars = Arc::clone(&self.dotenv_vars);
        let backend_child = Arc::clone(&self.backend_child);
        let app_slug = self.app_slug.clone();
        let app_entrypoint = self.app_entrypoint.clone();
        let host = self.host.clone();
        let backend_port = self.backend_port;
        let frontend_port = self.frontend_port;
        let db_port = self.db_port;
        let dev_server_port = self.dev_server_port;
        let dev_token = self.dev_token.clone();
        let db_password = self.db_password.clone();
        let dev_config = self.dev_config.clone();
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
                app_dir.join(".env"),
                app_dir.join("pyproject.toml"),
                app_dir.join("uv.lock"),
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

                    // Check if we received more events during the debounce period
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

                    // No more events, proceed with restart.
                    // Guard against concurrent restarts (e.g. rapid successive file changes).
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
                        if let Err(e) = crate::common::uv_sync(&app_dir).await {
                            warn!("uv sync failed: {}", e);
                            // Continue anyway - uvicorn may still work with existing deps
                        }
                    }

                    // Reload .env if it exists
                    let new_vars = if let Ok(dotenv) = DotenvFile::read(&app_dir.join(".env")) {
                        dotenv.get_vars()
                    } else {
                        HashMap::new()
                    };

                    // Stop the current backend process
                    Self::stop_child_tree_static("backend", &backend_child).await;

                    // Update dotenv vars
                    {
                        let mut vars = dotenv_vars.lock().await;
                        *vars = new_vars.clone();
                    }

                    // Restart uvicorn
                    if let Err(e) = Self::spawn_uvicorn_static(
                        &app_dir,
                        &app_slug,
                        &app_entrypoint,
                        &host,
                        backend_port,
                        frontend_port,
                        db_port,
                        dev_server_port,
                        &dev_token,
                        &db_password,
                        &dotenv_vars,
                        &backend_child,
                        &dev_config,
                    )
                    .await
                    {
                        warn!("Failed to restart backend: {}", e);
                    }
                    restarting.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
    }

    async fn spawn_process(
        &self,
        app_dir: &Path,
        executable: PathBuf,
        args: Vec<String>,
        source: LogSource,
        include_dotenv: bool,
    ) -> Result<Child, String> {
        let mut cmd = Command::new(executable);
        cmd.args(args)
            .current_dir(app_dir)
            .stdin(Stdio::null())
            // Capture stdout/stderr to forward to flux
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_env(&mut cmd, include_dotenv).await;

        let mut child = cmd
            .spawn()
            .map_err(|err| format!("Failed to start {source} process: {err}"))?;

        // Spawn tasks to read stdout/stderr, prefix with source, and forward to flux
        let service_name = format!("{}_{}", self.app_slug, source);
        let app_path = self.app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let service_name = service_name.clone();
            let app_path = app_path.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{}", format_log_line(source, &line));
                    forward_log_to_flux(&line, "INFO", &service_name, &app_path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{}", format_log_line(source, &line));
                    let severity = apx_common::format::parse_python_severity(&line);
                    forward_log_to_flux(&line, severity, &service_name, &app_path).await;
                }
            });
        }

        Ok(child)
    }

    /// Static version of stop_child_tree for use in async tasks without self
    async fn stop_child_tree_static(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            if let Some(pid) = pid {
                if let Err(err) = Self::kill_process_tree_async(pid, name.to_string()).await {
                    warn!(error = %err, process = name, pid, "Failed to kill process tree.");
                }
            } else {
                warn!(process = name, "Missing PID for child process.");
            }
            match timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => debug!(process = name, ?status, "Child process exited."),
                Ok(Err(err)) => {
                    warn!(error = %err, process = name, "Failed to wait for child process.")
                }
                Err(_) => warn!(
                    process = name,
                    "Timed out waiting for child process to exit."
                ),
            }
        } else {
            debug!(process = name, "No child process to stop.");
        }
    }

    /// Static version of spawn_uvicorn for use in async tasks without self
    #[allow(clippy::too_many_arguments)]
    async fn spawn_uvicorn_static(
        app_dir: &Path,
        app_slug: &str,
        app_entrypoint: &str,
        host: &str,
        backend_port: u16,
        frontend_port: Option<u16>,
        db_port: u16,
        dev_server_port: u16,
        dev_token: &str,
        db_password: &str,
        dotenv_vars: &Arc<Mutex<HashMap<String, String>>>,
        backend_child: &Arc<Mutex<Option<Child>>>,
        dev_config: &DevConfig,
    ) -> Result<(), String> {
        // ============================================================================
        // Backend logs are captured via stdout/stderr and forwarded to flux.
        // No OTEL Python dependencies required - apx handles log collection.
        // See spawn_uvicorn() for detailed explanation.
        // ============================================================================

        // Resolve uvicorn logging config (inline TOML, external Python file, or default)
        let log_config_result = resolve_log_config(dev_config, app_slug, app_dir).await?;

        // Validate JSON configs before use (skip validation for Python file configs)
        let log_config_str = match &log_config_result {
            LogConfigResult::PythonFile(path) => path.display().to_string(),
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
                    .cwd(app_dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .exec()
                    .await
                    .map_err(|e| format!("Failed to validate logging config: {e}"))?;

                if output.exit_code == Some(0) {
                    config_path.display().to_string()
                } else {
                    // Validation failed - log error and fall back to default config
                    let stderr = &output.stderr;
                    warn!(
                        "Logging config validation failed, falling back to default:\n{}",
                        stderr
                    );
                    eprintln!(
                        "⚠️  Custom logging config is invalid, using default config:\n{}",
                        stderr
                    );

                    // Generate and write default config
                    let default_config = default_logging_config(app_slug);
                    let fallback_path =
                        write_logging_config_json(&default_config, app_dir)
                            .await
                            .map_err(|e| format!("Failed to write fallback logging config: {e}"))?;
                    fallback_path.display().to_string()
                }
            }
        };

        // Run uvicorn via uv to ensure correct Python environment
        let mut tool_cmd = UvTool::new("uvicorn")
            .await?
            .cmd()
            .args([
                app_entrypoint,
                "--host",
                host,
                "--port",
                &backend_port.to_string(),
                "--reload",
                "--log-config",
                &log_config_str,
            ])
            .cwd(app_dir)
            .stdin(Stdio::null())
            // Capture stdout/stderr for prefixed logging and flux forwarding
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("APX_BACKEND_PORT", backend_port.to_string())
            .env("APX_DEV_DB_PORT", db_port.to_string())
            .env("APX_DEV_DB_PWD", db_password)
            .env("APX_DEV_SERVER_PORT", dev_server_port.to_string())
            .env("APX_DEV_SERVER_HOST", host)
            .env("APX_DEV_TOKEN", dev_token)
            // Force Python to flush stdout/stderr immediately (no buffering)
            .env("PYTHONUNBUFFERED", "1");

        if let Some(fp) = frontend_port {
            tool_cmd = tool_cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }

        let vars = dotenv_vars.lock().await;
        for (key, value) in vars.iter() {
            tool_cmd = tool_cmd.env(key, value);
        }
        drop(vars);

        let mut child = tool_cmd.spawn().map_err(String::from)?;

        // Spawn tasks to read stdout/stderr, prefix with source, and forward to flux
        let service_name = format!("{app_slug}_app");
        let app_path = app_dir.display().to_string();

        if let Some(stdout) = child.stdout.take() {
            let service_name = service_name.clone();
            let app_path = app_path.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{}", format_log_line(LogSource::App, &line));
                    forward_log_to_flux(&line, "INFO", &service_name, &app_path).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{}", format_log_line(LogSource::App, &line));
                    let severity = apx_common::format::parse_python_severity(&line);
                    forward_log_to_flux(&line, severity, &service_name, &app_path).await;
                }
            });
        }

        let mut guard = backend_child.lock().await;
        *guard = Some(child);

        Ok(())
    }

    async fn apply_env(&self, cmd: &mut Command, include_dotenv: bool) {
        if let Some(fp) = self.frontend_port {
            cmd.env("APX_FRONTEND_PORT", fp.to_string());
        }
        cmd.env("APX_BACKEND_PORT", self.backend_port.to_string());
        cmd.env("APX_DEV_DB_PORT", self.db_port.to_string());
        cmd.env("APX_DEV_DB_PWD", self.db_password.clone());
        cmd.env("APX_DEV_SERVER_PORT", self.dev_server_port.to_string());
        cmd.env("APX_DEV_SERVER_HOST", self.host.clone());
        cmd.env("APX_DEV_TOKEN", self.dev_token.clone());

        if include_dotenv {
            let vars = self.dotenv_vars.lock().await;
            for (key, value) in vars.iter() {
                cmd.env(key, value);
            }
        }
    }

    async fn ensure_bun() -> Result<Bun, String> {
        Bun::new().await
    }

    /// Send SIGTERM to a child process tree (polite shutdown request).
    /// On Windows, SIGTERM is not supported — falls back to SIGKILL (TerminateProcess).
    async fn send_sigterm(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let guard = child.lock().await;
        if let Some(child) = guard.as_ref()
            && let Some(pid) = child.id()
        {
            #[cfg(unix)]
            let signal = Signal::Term;
            #[cfg(not(unix))]
            let signal = Signal::Kill;
            debug!(process = name, pid, signal = ?signal, "Sending signal to process tree.");
            Self::send_signal_to_tree(pid, signal, name.to_string()).await;
        }
    }

    /// Wait for a child process to exit.
    async fn wait_for_child(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    debug!(process = name, ?status, "Child process already exited.");
                }
                Ok(None) => {
                    // Process still running, wait for it
                    match child.wait().await {
                        Ok(status) => debug!(process = name, ?status, "Child process exited."),
                        Err(err) => {
                            warn!(error = %err, process = name, "Failed to wait for child.")
                        }
                    }
                }
                Err(err) => warn!(error = %err, process = name, "Failed to check child status."),
            }
        }
    }

    /// Force kill a child process tree (SIGKILL).
    /// After the sysinfo-based tree kill, also uses tokio's cross-platform kill
    /// as a fallback (important on Windows where sysinfo's kill_with may fail).
    async fn force_kill(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            // Check if process is still running
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Already exited, nothing to do
                    debug!(
                        process = name,
                        "Process already exited, skipping force kill."
                    );
                }
                Ok(None) => {
                    // Still running, force kill via sysinfo tree walk
                    if let Some(pid) = child.id() {
                        debug!(process = name, pid, "Force killing process tree.");
                        Self::send_signal_to_tree(pid, Signal::Kill, name.to_string()).await;
                    }
                    // Fallback: use tokio's cross-platform kill for the direct child.
                    // This ensures termination even if sysinfo's kill_with fails (e.g. on Windows).
                    if let Err(err) = child.kill().await {
                        debug!(error = %err, process = name, "Tokio child.kill() fallback failed (process may have already exited).");
                    }
                    // Brief wait to allow kill to take effect
                    let _ = timeout(Duration::from_millis(100), child.wait()).await;
                }
                Err(err) => {
                    warn!(error = %err, process = name, "Failed to check process status.");
                }
            }
        }
    }

    /// Send a signal to an entire process tree. This is a blocking operation.
    fn send_signal_to_tree_blocking(pid: u32, signal: Signal, label: &str) {
        let root_pid = Pid::from_u32(pid);
        let mut sys = System::new_all();
        sys.refresh_all();

        let Some(root_process) = sys.process(root_pid) else {
            debug!(
                process = label,
                pid, "Process not found, may have already exited."
            );
            return;
        };

        let root_start_time = root_process.start_time();
        let parents = Self::build_parent_map(&sys);

        // Log the process tree we're about to signal
        debug!(
            process = label,
            root_pid = ?root_pid,
            root_name = ?root_process.name(),
            "Sending {:?} to process tree", signal
        );
        Self::log_process_tree(&sys, &parents, root_pid, root_start_time, label, 0);

        Self::send_signal_tree_recursive(&sys, &parents, root_pid, root_start_time, signal, label);
    }

    /// Log the process tree for debugging.
    fn log_process_tree(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        label: &str,
        depth: usize,
    ) {
        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time >= root_start_time {
                let indent = "  ".repeat(depth);
                debug!(
                    process = label,
                    "{}{:?} ({:?}) - started at {}",
                    indent,
                    pid,
                    process.name(),
                    process_start_time
                );
            }
        }

        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::log_process_tree(sys, parents, *child_pid, root_start_time, label, depth + 1);
            }
        }
    }

    /// Async wrapper for send_signal_to_tree that runs on a blocking thread.
    async fn send_signal_to_tree(pid: u32, signal: Signal, label: String) {
        let _ = tokio::task::spawn_blocking(move || {
            Self::send_signal_to_tree_blocking(pid, signal, &label)
        })
        .await;
    }

    /// Recursively send signal to process tree.
    fn send_signal_tree_recursive(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        signal: Signal,
        label: &str,
    ) {
        // First, signal all children
        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::send_signal_tree_recursive(
                    sys,
                    parents,
                    *child_pid,
                    root_start_time,
                    signal,
                    label,
                );
            }
        }

        // Then signal this process
        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time < root_start_time {
                return;
            }
            let name = process.name();
            if process.kill_with(signal).unwrap_or(false) {
                debug!(pid = ?pid, process_name = ?name, ?signal, process = label, "Sent signal to process.");
            }
        }
    }

    /// Stop a child process tree immediately (used for restart operations).
    async fn stop_child_tree(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            let pid = child.id();
            if let Some(pid) = pid {
                if let Err(err) = Self::kill_process_tree_async(pid, name.to_string()).await {
                    warn!(error = %err, process = name, pid, "Failed to kill process tree.");
                }
            } else {
                warn!(process = name, "Missing PID for child process.");
            }
            match timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => debug!(process = name, ?status, "Child process exited."),
                Ok(Err(err)) => {
                    warn!(error = %err, process = name, "Failed to wait for child process.")
                }
                Err(_) => warn!(
                    process = name,
                    "Timed out waiting for child process to exit."
                ),
            }
        } else {
            debug!(process = name, "No child process to stop.");
        }
    }

    /// Check the status of a process.
    /// If http_check is Some((host, port)), also performs an HTTP health probe.
    /// If http_check is None (for DB), just checks if the process is running.
    ///
    /// Returns:
    /// - "healthy": process is running and responding to HTTP probes (or running for DB)
    /// - "starting": process is running but not yet responding to HTTP probes
    /// - "stopped": process was never started or was explicitly stopped
    /// - "failed": process was started but has exited (crashed) - unrecoverable
    /// - "error": failed to check process status
    ///
    /// IMPORTANT: Mutex is released before HTTP probe to avoid blocking other operations.
    async fn status_for_process(
        &self,
        child: &Arc<Mutex<Option<Child>>>,
        http_check: Option<(&str, u16)>,
    ) -> String {
        // Quick mutex access to check process state - released before HTTP probe
        let process_running = {
            let mut guard = child.lock().await;
            match guard.as_mut() {
                // Process was never started or was explicitly stopped
                None => return "stopped".to_string(),
                Some(process) => match process.try_wait() {
                    // Process is still running
                    Ok(None) => true,
                    // Process has exited - this is a failure (crashed/error)
                    Ok(Some(_)) => return "failed".to_string(),
                    Err(_) => return "error".to_string(),
                },
            }
        }; // Mutex released here!

        // Process is running - for DB that's healthy, for others need HTTP check
        if !process_running {
            return "stopped".to_string();
        }

        match http_check {
            None => "healthy".to_string(), // DB: running = healthy
            Some((host, port)) => match Self::http_health_probe(host, port).await {
                ProbeResult::Responded(_) => "healthy".to_string(),
                ProbeResult::Failed(_) => "starting".to_string(),
            },
        }
    }

    /// Probe a service by making an HTTP GET request to its root path.
    /// Any HTTP response (regardless of status code) means the server is up.
    /// Only connection/timeout failures indicate the server isn't ready yet.
    async fn http_health_probe(host: &str, port: u16) -> ProbeResult {
        let url = format!("http://{host}:{port}/");
        let start = std::time::Instant::now();
        match HEALTH_CLIENT.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let elapsed_ms = start.elapsed().as_millis();
                if status == 200 {
                    debug!(url = %url, status, elapsed_ms, "Health probe OK");
                } else {
                    warn!(url = %url, status, elapsed_ms, "Health probe returned non-200");
                }
                ProbeResult::Responded(status)
            }
            Err(err) => {
                let elapsed_ms = start.elapsed().as_millis();
                debug!(url = %url, error = %err, elapsed_ms, "Health probe failed");
                ProbeResult::Failed(err.to_string())
            }
        }
    }

    fn generate_dev_token() -> String {
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect()
    }

    /// Kill a process tree. This is a blocking operation that should be called
    /// from a blocking context or wrapped in spawn_blocking.
    pub fn kill_process_tree(pid: u32, label: &str) -> Result<(), String> {
        let root_pid = Pid::from_u32(pid);
        let mut sys = System::new_all();
        sys.refresh_all();
        let root_process = sys
            .process(root_pid)
            .ok_or_else(|| format!("{label} process {pid} not found"))?;
        let root_start_time = root_process.start_time();
        let parents = Self::build_parent_map(&sys);
        debug!(
            pid = ?root_pid,
            root_start_time,
            process = label,
            "Killing process tree."
        );
        Self::kill_tree_with_guard(&sys, &parents, root_pid, root_start_time, label);
        Ok(())
    }

    /// Async wrapper for kill_process_tree that runs on a blocking thread.
    pub async fn kill_process_tree_async(pid: u32, label: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || Self::kill_process_tree(pid, &label))
            .await
            .map_err(|err| format!("Failed to spawn blocking task: {err}"))?
    }

    fn build_parent_map(sys: &System) -> HashMap<Pid, Vec<Pid>> {
        let mut parents: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in sys.processes() {
            if let Some(parent) = process.parent() {
                parents.entry(parent).or_default().push(*pid);
            }
        }
        parents
    }

    fn kill_tree_with_guard(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        label: &str,
    ) {
        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::kill_tree_with_guard(sys, parents, *child_pid, root_start_time, label);
            }
        }

        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time < root_start_time {
                debug!(
                    pid = ?pid,
                    process_start_time,
                    root_start_time,
                    process = label,
                    "Skipping process because it predates the root."
                );
                return;
            }
            let name = process.name();
            match process.kill_with(Signal::Kill) {
                Some(true) => {
                    debug!(pid = ?pid, process_name = ?name, process = label, "Killed process.");
                }
                Some(false) => {
                    warn!(pid = ?pid, process_name = ?name, process = label, "kill_with(SIGKILL) returned false — process may require elevated privileges.");
                }
                None => {
                    warn!(pid = ?pid, process_name = ?name, process = label, "kill_with(SIGKILL) not supported on this platform for this process.");
                }
            }
        }
    }
}
