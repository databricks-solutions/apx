use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::common::{
    ApxCommand, OutputMode, emit, ensure_dir, format_elapsed_ms, handle_spawn_error,
    run_preflight_checks, spinner_for_mode,
};
use crate::dev::client::{HealthCheckConfig, health, stop as stop_server};
use crate::dev::common::{
    DevLock, is_process_running, lock_path, read_lock, remove_lock, write_lock,
};
use crate::dev::process::ProcessManager;
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

/// Spawn a new dev server subprocess (does not check for existing server).
pub async fn spawn_server(
    app_dir: &Path,
    preferred_port: Option<u16>,
    skip_credentials_validation: bool,
    timeout_secs: u64,
    skip_healthcheck: bool,
    mode: OutputMode,
) -> Result<u16, String> {
    let start_time = Instant::now();
    prepare_app_dir(app_dir)?;

    run_preflight(app_dir, mode).await?;

    let lock_path = lock_path(app_dir);

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

    let apx_cmd = ApxCommand::new().await?;

    let command = format!(
        "{} dev __internal__run_server --app-dir {} --host {} --port {}{}",
        apx_cmd.display(),
        app_dir.display(),
        BIND_HOST,
        port,
        if skip_credentials_validation {
            " --skip-credentials-validation"
        } else {
            ""
        }
    );

    let mut cmd = apx_cmd.tokio_command();
    cmd.arg("dev")
        .arg("__internal__run_server")
        .arg("--app-dir")
        .arg(app_dir)
        .arg("--host")
        .arg(BIND_HOST)
        .arg("--port")
        .arg(port.to_string());

    if skip_credentials_validation {
        cmd.arg("--skip-credentials-validation");
    }

    let canonical_app_dir = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf());

    let mut child = cmd
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("APX_OTEL_LOGS", "1")
        .env("APX_APP_DIR", &canonical_app_dir)
        .spawn()
        .map_err(|err| handle_spawn_error("apx", err))?;

    if skip_healthcheck {
        let pid = child.id().ok_or("Failed to get child process ID")?;
        let lock = DevLock::new(pid, port, command, app_dir);
        write_lock(&lock_path, &lock)?;

        emit(
            mode,
            &format!(
                "✅ Dev server started at http://{BROWSER_HOST}:{port} in {} (healthcheck skipped)\n",
                format_elapsed_ms(start_time)
            ),
        );
        return Ok(port);
    }

    emit(mode, "⏳ Waiting for dev server to become healthy...\n");
    let config = HealthCheckConfig {
        timeout_secs,
        ..HealthCheckConfig::default()
    };

    let health_result = wait_for_healthy_with_logs(port, &config, app_dir, mode).await;

    if let Err(e) = health_result {
        debug!("Health checks failed, attempting graceful shutdown.");
        let shutdown_result = tokio::time::timeout(Duration::from_secs(5), stop_server(port)).await;

        match shutdown_result {
            Ok(Ok(())) => debug!("Graceful shutdown completed."),
            Ok(Err(err)) => debug!("Graceful shutdown failed: {}", err),
            Err(_) => debug!("Graceful shutdown timed out."),
        }

        if let Some(pid) = child.id() {
            let _ = ProcessManager::kill_process_tree_async(pid, "dev-server".to_string()).await;
        }
        drop(child.kill());

        let _ = remove_lock(&lock_path);

        if let Ok(logs) = crate::ops::logs::fetch_logs(app_dir, "30s").await {
            let logs = logs.trim();
            if !logs.is_empty() {
                eprintln!("\n📋 Recent logs:\n{logs}\n");
            }
        }

        return Err(e);
    }

    let pid = child.id().ok_or("Failed to get child process ID")?;
    let lock = DevLock::new(pid, port, command, app_dir);
    write_lock(&lock_path, &lock)?;

    emit(
        mode,
        &format!(
            "✅ Dev server started at http://{BROWSER_HOST}:{port} in {}\n",
            format_elapsed_ms(start_time)
        ),
    );
    Ok(port)
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

    match stop_server(lock.port).await {
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

    let kill_result = ProcessManager::kill_process_tree(lock.pid, "dev-server");
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
