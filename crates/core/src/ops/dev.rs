use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::app_state::set_app_dir;
use crate::common::{
    OutputMode, emit, ensure_dir, format_elapsed_ms, read_project_metadata, run_preflight_checks,
    spinner_for_mode,
};
use crate::dev::client::{HealthCheckConfig, health, stop as stop_server};
use crate::dev::common::{
    BACKEND_PORT_END, BACKEND_PORT_START, DB_PORT_END, DB_PORT_START, DevLock, FRONTEND_PORT_END,
    FRONTEND_PORT_START, find_random_port_in_range, is_process_running, lock_path, read_lock,
    remove_lock, write_lock,
};
use crate::dev::server::{ServerConfig, run_server};
use crate::dev::token;
use crate::external::uv::ApxTool;
use crate::flux;
use crate::ops::healthcheck::wait_for_healthy_with_logs;
use crate::registry::Registry;
use apx_common::hosts::{BIND_HOST, BROWSER_HOST};
use tracing::{debug, warn};

/// Prepare the app directory for dev server startup.
fn prepare_app_dir(app_dir: &Path) -> Result<(), String> {
    ensure_dir(&app_dir.join(".apx"))?;
    Ok(())
}

pub async fn resolve_existing_server(
    app_dir: &Path,
    mode: OutputMode,
) -> Result<Option<u16>, String> {
    let lock_path = lock_path(app_dir);
    if !lock_path.exists() {
        return Ok(None);
    }

    let lock = read_lock(&lock_path)?;

    if !is_process_running(lock.pid) {
        emit(mode, "🧹 Cleaning up stale lock file...");
        remove_lock(&lock_path)?;
        return Ok(None);
    }

    match health(lock.port).await {
        Ok(true) => Ok(Some(lock.port)),
        Ok(false) | Err(_) => {
            emit(mode, "🧹 Cleaning up stale lock file...");
            remove_lock(&lock_path)?;
            Ok(None)
        }
    }
}

/// Start a dev server for the given app directory.
/// If a server is already running and healthy, returns its port.
/// Otherwise spawns a new server subprocess.
pub async fn start_dev_server(
    app_dir: &Path,
    skip_healthcheck: bool,
    mode: OutputMode,
) -> Result<u16, String> {
    if let Some(port) = resolve_existing_server(app_dir, mode).await? {
        emit(
            mode,
            &format!("Dev server is already running at http://{BROWSER_HOST}:{port}\n"),
        );
        return Ok(port);
    }
    spawn_server(app_dir, None, false, 60, skip_healthcheck, mode).await
}

/// Run preflight checks and display progress.
async fn run_preflight(app_dir: &Path, mode: OutputMode) -> Result<(), String> {
    emit(mode, "🛫 Preflight check started...");
    let preflight_start = Instant::now();

    let preflight_spinner = spinner_for_mode("  Running preflight checks...", mode);

    let result = run_preflight_checks(app_dir).await;
    preflight_spinner.finish_and_clear();

    match result {
        Ok(preflight) => {
            emit(
                mode,
                &format!("  ✓ verified project layout ({}ms)", preflight.layout_ms),
            );
            emit(mode, &format!("  ✓ uv sync ({}ms)", preflight.uv_sync_ms));
            emit(
                mode,
                &format!("  ✓ version file ({}ms)", preflight.version_ms),
            );
            if preflight.has_ui {
                if let Some(bun_ms) = preflight.bun_install_ms {
                    emit(mode, &format!("  ✓ bun install ({bun_ms}ms)"));
                } else {
                    emit(mode, "  ✓ node_modules (cached)");
                }
            }
            emit(
                mode,
                &format!(
                    "✅ Ready for takeoff! ({})\n",
                    format_elapsed_ms(preflight_start)
                ),
            );
            Ok(())
        }
        Err(e) => {
            emit(mode, "❌ Preflight check failed\n");
            Err(e)
        }
    }
}

/// Maximum time to wait for a port to become available (in ms).
const PORT_WAIT_TIMEOUT_MS: u64 = 2000;
/// Interval between port availability checks (in ms).
const PORT_WAIT_INTERVAL_MS: u64 = 100;

/// Wait for a port to become available, with timeout.
async fn wait_for_port_available(port: u16, mode: OutputMode) -> Result<(), String> {
    let max_attempts = PORT_WAIT_TIMEOUT_MS / PORT_WAIT_INTERVAL_MS;
    for attempt in 0..max_attempts {
        if TcpListener::bind((BIND_HOST, port)).is_ok() {
            return Ok(());
        }
        if attempt == 0 {
            emit(
                mode,
                &format!("⏳ Waiting for port {port} to become available..."),
            );
        }
        tokio::time::sleep(Duration::from_millis(PORT_WAIT_INTERVAL_MS)).await;
    }
    Err(format!(
        "Port {port} is still in use after {PORT_WAIT_TIMEOUT_MS}ms. Another process may be using it."
    ))
}

// ---------------------------------------------------------------------------
// PreparedServer — shared launch preparation
// ---------------------------------------------------------------------------

/// Immutable result of server launch preparation.
/// Shared by all `ServerLauncher` implementations.
#[derive(Debug)]
pub struct PreparedServer {
    pub port: u16,
    pub dev_token: String,
    pub lock_path: PathBuf,
    pub canonical_app_dir: PathBuf,
    pub command_display: String,
}

/// Run preflight checks, start flux, allocate a stable port.
/// Returns a `PreparedServer` ready for any launch mode.
pub async fn prepare_server_launch(
    app_dir: &Path,
    preferred_port: Option<u16>,
    mode: OutputMode,
) -> Result<PreparedServer, String> {
    prepare_app_dir(app_dir)?;
    run_preflight(app_dir, mode).await?;

    emit(mode, "🚀 Starting dev server...");

    if let Err(e) = flux::ensure_running() {
        debug!("Failed to start flux: {e}. Logs may not be collected.");
    }

    let mut registry = Registry::load()?;
    let stale = registry.cleanup_stale_entries();
    if !stale.is_empty() {
        debug!("Cleaned up {} stale registry entries", stale.len());
    }

    let port = registry.get_or_allocate_port(app_dir, preferred_port)?;
    registry.save()?;

    wait_for_port_available(port, mode).await?;

    let dev_token = token::generate();
    let canonical_app_dir = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf());

    Ok(PreparedServer {
        port,
        dev_token,
        lock_path: lock_path(app_dir),
        canonical_app_dir,
        command_display: format!("apx dev (port {port})"),
    })
}

// ---------------------------------------------------------------------------
// ServerLauncher — enum-based launch strategy
// ---------------------------------------------------------------------------

/// Strategy for launching and running the dev server.
/// Two variants cover the fixed set of launch modes.
#[derive(Debug)]
pub enum ServerLauncher {
    /// Spawns a background child process (`apx dev __internal__run_server`).
    /// Returns after healthcheck confirms the server is ready.
    Detached {
        app_dir: PathBuf,
        skip_credentials_validation: bool,
        timeout_secs: u64,
        skip_healthcheck: bool,
        mode: OutputMode,
    },
    /// Runs the Axum dev server in-process as an async task.
    /// Subprocess logs stream directly to the terminal. Returns after shutdown.
    Attached {
        app_dir: PathBuf,
        skip_credentials_validation: bool,
    },
}

/// Result of a server launch.
#[derive(Debug)]
pub enum LaunchOutcome {
    /// Server is running in the background. Port is ready.
    Running { port: u16 },
    /// Server ran in-process and has shut down.
    Shutdown,
}

impl ServerLauncher {
    pub async fn launch(self, server: PreparedServer) -> Result<LaunchOutcome, String> {
        match self {
            Self::Detached {
                app_dir,
                skip_credentials_validation,
                timeout_secs,
                skip_healthcheck,
                mode,
            } => {
                launch_detached(
                    &app_dir,
                    skip_credentials_validation,
                    timeout_secs,
                    skip_healthcheck,
                    mode,
                    server,
                )
                .await
            }
            Self::Attached {
                app_dir,
                skip_credentials_validation,
            } => launch_attached(&app_dir, skip_credentials_validation, server).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Detached mode — spawns a background child process
// ---------------------------------------------------------------------------

async fn launch_detached(
    app_dir: &Path,
    skip_credentials_validation: bool,
    timeout_secs: u64,
    skip_healthcheck: bool,
    mode: OutputMode,
    server: PreparedServer,
) -> Result<LaunchOutcome, String> {
    let start_time = Instant::now();
    let (command, mut child) =
        spawn_detached_child(app_dir, skip_credentials_validation, &server).await?;

    if skip_healthcheck {
        return finalize_skip_healthcheck(app_dir, mode, &server, &command, &mut child, start_time);
    }

    wait_for_healthy_or_cleanup(
        app_dir,
        mode,
        timeout_secs,
        &server,
        &command,
        &mut child,
        start_time,
    )
    .await
}

/// Build and spawn the `apx dev __internal__run_server` subprocess.
async fn spawn_detached_child(
    app_dir: &Path,
    skip_credentials_validation: bool,
    server: &PreparedServer,
) -> Result<(String, tokio::process::Child), String> {
    let apx_cmd = ApxTool::new_apx().await?;

    let command = format!(
        "{} dev __internal__run_server --app-dir {} --host {} --port {}{}",
        apx_cmd.display(),
        app_dir.display(),
        BIND_HOST,
        server.port,
        if skip_credentials_validation {
            " --skip-credentials-validation"
        } else {
            ""
        }
    );

    let mut tool_cmd = apx_cmd
        .cmd()
        .arg("dev")
        .arg("__internal__run_server")
        .arg("--app-dir")
        .arg(app_dir)
        .arg("--host")
        .arg(BIND_HOST)
        .arg("--port")
        .arg(server.port.to_string());

    if skip_credentials_validation {
        tool_cmd = tool_cmd.arg("--skip-credentials-validation");
    }

    let child = tool_cmd
        .cwd(app_dir)
        .env("APX_OTEL_LOGS", "1")
        .env("APX_APP_DIR", &server.canonical_app_dir)
        .env(token::DEV_TOKEN_ENV, &server.dev_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(String::from)?;

    Ok((command, child))
}

/// Write lock file and return immediately (no healthcheck).
fn finalize_skip_healthcheck(
    app_dir: &Path,
    mode: OutputMode,
    server: &PreparedServer,
    command: &str,
    child: &mut tokio::process::Child,
    start_time: Instant,
) -> Result<LaunchOutcome, String> {
    let pid = child.id().ok_or("Failed to get child process ID")?;
    let lock = DevLock::new(
        pid,
        server.port,
        command.to_string(),
        app_dir,
        server.dev_token.clone(),
    );
    write_lock(&server.lock_path, &lock)?;

    emit(
        mode,
        &format!(
            "✅ Dev server started at http://{BROWSER_HOST}:{port} in {} (healthcheck skipped)\n",
            format_elapsed_ms(start_time),
            port = server.port,
        ),
    );
    Ok(LaunchOutcome::Running { port: server.port })
}

/// Run healthcheck, handle failure with cleanup, or finalize on success.
async fn wait_for_healthy_or_cleanup(
    app_dir: &Path,
    mode: OutputMode,
    timeout_secs: u64,
    server: &PreparedServer,
    command: &str,
    child: &mut tokio::process::Child,
    start_time: Instant,
) -> Result<LaunchOutcome, String> {
    emit(mode, "⏳ Waiting for dev server to become healthy...\n");
    let config = HealthCheckConfig {
        timeout_secs,
        ..HealthCheckConfig::default()
    };

    let health_result = wait_for_healthy_with_logs(server.port, &config, app_dir, mode).await;

    if let Err(e) = health_result {
        cleanup_failed_child(app_dir, server, child).await;
        return Err(e);
    }

    let pid = child.id().ok_or("Failed to get child process ID")?;
    let lock = DevLock::new(
        pid,
        server.port,
        command.to_string(),
        app_dir,
        server.dev_token.clone(),
    );
    write_lock(&server.lock_path, &lock)?;

    emit(
        mode,
        &format!(
            "✅ Dev server started at http://{BROWSER_HOST}:{port} in {}\n",
            format_elapsed_ms(start_time),
            port = server.port,
        ),
    );
    Ok(LaunchOutcome::Running { port: server.port })
}

/// Attempt graceful shutdown, kill process tree, remove lock, show recent logs.
async fn cleanup_failed_child(
    app_dir: &Path,
    server: &PreparedServer,
    child: &mut tokio::process::Child,
) {
    debug!("Health checks failed, attempting graceful shutdown.");

    let shutdown_result = tokio::time::timeout(
        Duration::from_secs(5),
        stop_server(server.port, Some(&server.dev_token)),
    )
    .await;

    match shutdown_result {
        Ok(Ok(())) => debug!("Graceful shutdown completed."),
        Ok(Err(err)) => debug!("Graceful shutdown failed: {}", err),
        Err(_) => debug!("Graceful shutdown timed out."),
    }

    if let Some(pid) = child.id() {
        let _ = crate::dev::common::kill_process_tree_async(pid, "dev-server".to_string()).await;
    }
    drop(child.kill());

    let _ = remove_lock(&server.lock_path);

    if let Ok(logs) = crate::ops::logs::fetch_logs(app_dir, "30s").await {
        let logs = logs.trim();
        if !logs.is_empty() {
            eprintln!("\n📋 Recent logs:\n{logs}\n");
        }
    }
}

// ---------------------------------------------------------------------------
// Attached mode — runs the server in-process
// ---------------------------------------------------------------------------

/// Maximum number of retries for subprocess port allocation.
const MAX_PORT_RETRIES: u32 = 5;

async fn launch_attached(
    app_dir: &Path,
    skip_credentials_validation: bool,
    server: PreparedServer,
) -> Result<LaunchOutcome, String> {
    set_app_dir(app_dir.to_path_buf())?;
    validate_credentials(skip_credentials_validation).await;

    let mut last_error = String::new();

    for attempt in 1..=MAX_PORT_RETRIES {
        let config = build_attached_server_config(app_dir, &server, attempt)?;

        // Write lock file with current process PID
        let pid = std::process::id();
        let lock = DevLock::new(
            pid,
            server.port,
            server.command_display.clone(),
            app_dir,
            server.dev_token.clone(),
        );
        write_lock(&server.lock_path, &lock)?;

        match run_server(config).await {
            Ok(()) => return Ok(LaunchOutcome::Shutdown),
            Err(e) if is_port_error(&e) && attempt < MAX_PORT_RETRIES => {
                warn!(attempt, error = %e, "Subprocess port conflict, retrying with new ports");
                last_error = e;
                continue;
            }
            Err(e) => {
                let _ = remove_lock(&server.lock_path);
                return Err(e);
            }
        }
    }

    let _ = remove_lock(&server.lock_path);
    Err(format!(
        "Failed to start dev server after {MAX_PORT_RETRIES} attempts. Last error: {last_error}"
    ))
}

/// Warn if credentials are missing or invalid.
async fn validate_credentials(skip: bool) {
    if skip {
        warn!("Credentials validation skipped. API proxy may not work correctly.");
        return;
    }
    let profile = std::env::var("DATABRICKS_CONFIG_PROFILE").unwrap_or_default();
    if let Err(err) = apx_databricks_sdk::validate_credentials(&profile).await {
        warn!("Credentials validation failed: {err}. API proxy may not work correctly.");
    }
}

/// Bind listener and pick random subprocess ports for one attempt.
fn build_attached_server_config(
    app_dir: &Path,
    server: &PreparedServer,
    attempt: u32,
) -> Result<ServerConfig, String> {
    let std_listener = TcpListener::bind((BIND_HOST, server.port))
        .map_err(|e| format!("Failed to bind main server port {}: {e}", server.port))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set listener to non-blocking: {e}"))?;
    let listener = tokio::net::TcpListener::from_std(std_listener)
        .map_err(|e| format!("Failed to convert to tokio listener: {e}"))?;

    let backend_port = find_random_port_in_range(BIND_HOST, BACKEND_PORT_START, BACKEND_PORT_END)?;
    let db_port = find_random_port_in_range(BIND_HOST, DB_PORT_START, DB_PORT_END)?;

    let metadata = read_project_metadata(app_dir)?;
    let frontend_port = if metadata.has_ui() {
        Some(find_random_port_in_range(
            BIND_HOST,
            FRONTEND_PORT_START,
            FRONTEND_PORT_END,
        )?)
    } else {
        None
    };

    debug!(
        attempt,
        backend_port,
        ?frontend_port,
        db_port,
        "Attempting to start dev server with ports"
    );

    Ok(ServerConfig {
        app_dir: app_dir.to_path_buf(),
        listener,
        backend_port,
        frontend_port,
        db_port,
        dev_token: server.dev_token.clone(),
    })
}

fn is_port_error(e: &str) -> bool {
    e.contains("address already in use") || e.contains("EADDRINUSE") || e.contains("not ready on")
}

// ---------------------------------------------------------------------------
// spawn_server — backward-compatible entry point (delegates to Detached)
// ---------------------------------------------------------------------------

/// Spawn a new dev server subprocess (does not check for existing server).
pub async fn spawn_server(
    app_dir: &Path,
    preferred_port: Option<u16>,
    skip_credentials_validation: bool,
    timeout_secs: u64,
    skip_healthcheck: bool,
    mode: OutputMode,
) -> Result<u16, String> {
    let server = prepare_server_launch(app_dir, preferred_port, mode).await?;
    let launcher = ServerLauncher::Detached {
        app_dir: app_dir.to_path_buf(),
        skip_credentials_validation,
        timeout_secs,
        skip_healthcheck,
        mode,
    };
    match launcher.launch(server).await? {
        LaunchOutcome::Running { port } => Ok(port),
        LaunchOutcome::Shutdown => unreachable!("Detached mode always returns Running"),
    }
}

/// Stop the dev server for the given app directory.
/// Returns true if a server was found and stopped, false if no server was running.
pub async fn stop_dev_server(app_dir: &Path, mode: OutputMode) -> Result<bool, String> {
    let lock_path = lock_path(app_dir);
    debug!(path = %lock_path.display(), "Checking for dev server lockfile.");
    if !lock_path.exists() {
        debug!("No dev server lockfile found.");
        emit(mode, "⚠️  No dev server running\n");
        return Ok(false);
    }

    let lock = read_lock(&lock_path)?;
    debug!(
        port = lock.port,
        pid = lock.pid,
        "Loaded dev server lockfile."
    );

    let start_time = Instant::now();
    let stop_spinner = spinner_for_mode("Stopping dev server...", mode);

    match stop_server(lock.port, lock.token.as_deref()).await {
        Ok(()) => {
            debug!("Dev server stopped gracefully via HTTP.");
            stop_spinner.finish_and_clear();
            emit(
                mode,
                &format!(
                    "✅ Dev server stopped in {}\n",
                    format_elapsed_ms(start_time)
                ),
            );
            return Ok(true);
        }
        Err(err) => {
            warn!(error = %err, "Graceful stop failed, falling back to process kill.");
        }
    }

    let kill_result = crate::dev::common::kill_process_tree(lock.pid, "dev-server");
    stop_spinner.finish_and_clear();
    match kill_result {
        Ok(()) => {
            debug!("Dev server process tree killed; removing lockfile.");
            remove_lock(&lock_path)?;
            emit(
                mode,
                &format!(
                    "✅ Dev server stopped in {}\n",
                    format_elapsed_ms(start_time)
                ),
            );
            Ok(true)
        }
        Err(err) => {
            warn!(error = %err, pid = lock.pid, "Failed to kill dev server process tree.");
            remove_lock(&lock_path)?;
            emit(mode, "✅ Dev server already stopped\n");
            Ok(true)
        }
    }
}

/// Restart the dev server for the given app directory.
/// Preserves the port if an existing server is found.
pub async fn restart_dev_server(
    app_dir: &Path,
    skip_healthcheck: bool,
    mode: OutputMode,
) -> Result<u16, String> {
    let lock_path = lock_path(app_dir);
    let preferred_port = if lock_path.exists() {
        let lock = read_lock(&lock_path)?;
        emit(
            mode,
            &format!(
                "Found existing dev server at http://{BROWSER_HOST}:{port}",
                port = lock.port
            ),
        );
        stop_dev_server(app_dir, mode).await?;
        Some(lock.port)
    } else {
        None
    };

    let port = spawn_server(app_dir, preferred_port, false, 60, skip_healthcheck, mode).await?;
    emit(
        mode,
        &format!("✅ Dev server restarted at http://{BROWSER_HOST}:{port}\n"),
    );
    Ok(port)
}
