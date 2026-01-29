use clap::Args;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::cli::dev::logs::LogsArgs;
// StartupLogStreamer is imported locally in wait_for_healthy_with_logs
use crate::cli::dev::stop::stop_dev_server;
use crate::cli::run_cli_async;
use crate::common::{
    ApxCommand, ensure_dir, format_elapsed_ms, handle_spawn_error, run_preflight_checks, spinner,
};
use crate::dev::client::{HealthCheckConfig, health, status, stop};
use crate::dev::common::{
    BIND_HOST, DevLock, is_process_running, lock_path, read_lock, remove_lock, write_lock,
};
use crate::dev::process::ProcessManager;
use crate::flux;
use crate::registry::Registry;
use tracing::debug;

/// Prepare the app directory for dev server startup.
/// Ensures the .apx directory exists.
fn prepare_app_dir(app_dir: &Path) -> Result<(), String> {
    ensure_dir(&app_dir.join(".apx"))?;
    Ok(())
}

#[derive(Args, Debug, Clone)]
pub struct StartArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        short = 'a',
        long = "attached",
        help = "Follow logs and stop server on Ctrl+C"
    )]
    pub attached: bool,
    #[arg(
        long = "skip-credentials-validation",
        help = "Skip credentials validation on startup (server will start but API proxy may not work)"
    )]
    pub skip_credentials_validation: bool,
    #[arg(
        long = "timeout",
        default_value = "60",
        value_name = "SECONDS",
        help = "Maximum time in seconds to wait for dev server to become healthy"
    )]
    pub timeout: u64,
}

pub async fn run(args: StartArgs) -> i32 {
    run_cli_async(|| async {
        if args.attached {
            run_attached(args).await
        } else {
            run_detached(args).await
        }
    })
    .await
}

async fn run_detached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    if let Some(port) = resolve_existing_server(&app_dir).await? {
        println!("✅ Dev server already running at http://localhost:{port}\n");
        return Ok(());
    }

    let _ = spawn_server(
        &app_dir,
        None,
        args.skip_credentials_validation,
        args.timeout,
    )
    .await?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    let port = if let Some(port) = resolve_existing_server(&app_dir).await? {
        println!("✅ Dev server already running at http://localhost:{port}, attaching logs...\n");
        port
    } else {
        spawn_server(
            &app_dir,
            None,
            args.skip_credentials_validation,
            args.timeout,
        )
        .await?
    };

    // Use the SQLite-based log following (reads from flux storage)
    let logs_args = LogsArgs {
        app_path: Some(app_dir.clone()),
        duration: "10m".to_string(),
        follow: true,
    };

    // Run logs command (will return on Ctrl+C)
    let _ = crate::cli::dev::logs::run(logs_args).await;

    // Ignore port to avoid unused warning
    let _ = port;

    stop_dev_server(&app_dir).await?;
    Ok(())
}

fn resolve_app_dir(args: &StartArgs) -> PathBuf {
    args.app_path
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

async fn resolve_existing_server(app_dir: &Path) -> Result<Option<u16>, String> {
    let lock_path = lock_path(app_dir);
    if !lock_path.exists() {
        return Ok(None);
    }

    let lock = read_lock(&lock_path)?;

    // First check: is the process still running?
    if !is_process_running(lock.pid) {
        println!("🧹 Cleaning up stale lock file...");
        remove_lock(&lock_path)?;
        return Ok(None);
    }

    // Second check: is the server responding?
    match health(lock.port).await {
        Ok(true) => Ok(Some(lock.port)),
        Ok(false) | Err(_) => {
            // Server process exists but not responding - could be zombie or different process reusing PID
            println!("🧹 Cleaning up stale lock file...");
            remove_lock(&lock_path)?;
            Ok(None)
        }
    }
}

/// Start a dev server for the given app directory.
/// If a server is already running and healthy, returns its port.
/// Otherwise spawns a new server subprocess.
pub async fn start_dev_server(app_dir: &Path) -> Result<u16, String> {
    if let Some(port) = resolve_existing_server(app_dir).await? {
        return Ok(port);
    }
    spawn_server(app_dir, None, false, 60).await
}

/// Run preflight checks and display progress.
async fn run_preflight(app_dir: &Path) -> Result<(), String> {
    println!("🛫 Preflight check started...");
    let preflight_start = Instant::now();

    let preflight_spinner = spinner("  Running preflight checks...");

    let result = run_preflight_checks(app_dir).await;
    preflight_spinner.finish_and_clear();

    match result {
        Ok(preflight) => {
            println!("  ✓ verified project layout ({}ms)", preflight.layout_ms);
            println!("  ✓ uv sync ({}ms)", preflight.uv_sync_ms);
            println!("  ✓ version file ({}ms)", preflight.version_ms);
            if let Some(bun_ms) = preflight.bun_install_ms {
                println!("  ✓ bun install ({bun_ms}ms)");
            } else {
                println!("  ✓ node_modules (cached)");
            }
            println!(
                "✅ Ready for takeoff! ({})\n",
                format_elapsed_ms(preflight_start)
            );
            Ok(())
        }
        Err(e) => {
            println!("❌ Preflight check failed\n");
            Err(e)
        }
    }
}

/// Spawn a new dev server subprocess (does not check for existing server).
pub(crate) async fn spawn_server(
    app_dir: &Path,
    preferred_port: Option<u16>,
    skip_credentials_validation: bool,
    timeout_secs: u64,
) -> Result<u16, String> {
    let start_time = Instant::now();
    prepare_app_dir(app_dir)?;

    // Run preflight checks (generates _metadata.py, _version.py, installs deps)
    run_preflight(app_dir).await?;

    let lock_path = lock_path(app_dir);

    println!("🚀 Starting dev server...");

    // Start flux for log collection (before subprocess so it's ready to receive logs)
    if let Err(e) = flux::ensure_running() {
        debug!("Failed to start flux: {e}. Logs may not be collected.");
    }

    // Load registry and cleanup stale entries (projects that no longer exist)
    let mut registry = Registry::load()?;
    let stale = registry.cleanup_stale_entries();
    if !stale.is_empty() {
        debug!("Cleaned up {} stale registry entries", stale.len());
    }

    // Get or allocate port from registry
    let port = registry.get_or_allocate_port(app_dir, preferred_port)?;
    registry.save()?;

    // Ensure the port is available (wait if needed)
    wait_for_port_available(port).await?;

    // Spawn apx via uv to ensure correct Python environment
    let apx_cmd = ApxCommand::new();

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

    // Canonicalize app_dir for consistent path matching in logs
    let canonical_app_dir = app_dir
        .canonicalize()
        .unwrap_or_else(|_| app_dir.to_path_buf());

    let mut child = cmd
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("APX_COLLECT_LOGS", "1")
        .env("APX_OTEL_LOGS", "1")
        .env("APX_APP_DIR", &canonical_app_dir)
        .spawn()
        .map_err(|err| handle_spawn_error("apx", err))?;

    // Wait for server to become healthy with inline log display
    println!("⏳ Waiting for dev server to become healthy...\n");
    let config = HealthCheckConfig {
        timeout_secs,
        ..HealthCheckConfig::default()
    };

    let health_result = wait_for_healthy_with_logs(port, &config, app_dir).await;

    if let Err(e) = health_result {
        // Try graceful shutdown (5 second timeout)
        debug!("Health checks failed, attempting graceful shutdown.");
        let shutdown_result = tokio::time::timeout(Duration::from_secs(5), stop(port)).await;

        match shutdown_result {
            Ok(Ok(())) => debug!("Graceful shutdown completed."),
            Ok(Err(err)) => debug!("Graceful shutdown failed: {}", err),
            Err(_) => debug!("Graceful shutdown timed out."),
        }

        // Kill any remaining processes (in case graceful shutdown failed or timed out)
        if let Some(pid) = child.id() {
            let _ = ProcessManager::kill_process_tree_async(pid, "dev-server".to_string()).await;
        }
        drop(child.kill()); // Fallback in case tree kill missed the root

        // Clean up lock file if it exists
        let _ = remove_lock(&lock_path);

        // Fetch and display recent logs from Flux on failure
        if let Ok(logs) = crate::cli::dev::logs::fetch_logs(app_dir, "30s").await {
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

    println!(
        "✅ Dev server started at http://localhost:{port} in {}\n",
        format_elapsed_ms(start_time)
    );
    Ok(port)
}

/// Maximum time to wait for a port to become available (in ms).
const PORT_WAIT_TIMEOUT_MS: u64 = 2000;
/// Interval between port availability checks (in ms).
const PORT_WAIT_INTERVAL_MS: u64 = 100;

/// Wait for a port to become available, with timeout.
/// Returns Ok if port is available, Err if timeout exceeded.
async fn wait_for_port_available(port: u16) -> Result<(), String> {
    let max_attempts = PORT_WAIT_TIMEOUT_MS / PORT_WAIT_INTERVAL_MS;
    for attempt in 0..max_attempts {
        if TcpListener::bind((BIND_HOST, port)).is_ok() {
            return Ok(());
        }
        if attempt == 0 {
            println!("⏳ Waiting for port {port} to become available...");
        }
        tokio::time::sleep(Duration::from_millis(PORT_WAIT_INTERVAL_MS)).await;
    }
    Err(format!(
        "Port {port} is still in use after {PORT_WAIT_TIMEOUT_MS}ms. Another process may be using it."
    ))
}

/// Wait for dev server to become healthy while streaming logs line-by-line.
async fn wait_for_healthy_with_logs(
    port: u16,
    config: &HealthCheckConfig,
    app_dir: &Path,
) -> Result<(), String> {
    use crate::cli::dev::startup_logs::StartupLogStreamer;

    // Give server time to start Python/tokio before polling
    debug!(
        "Starting health check with config: timeout={}s, retry_delay={}ms, initial_delay={}ms",
        config.timeout_secs, config.retry_delay_ms, config.initial_delay_ms
    );
    tokio::time::sleep(Duration::from_millis(config.initial_delay_ms)).await;

    let start_time = Instant::now();
    let deadline = start_time + Duration::from_secs(config.timeout_secs);
    let mut log_streamer = StartupLogStreamer::new(app_dir);
    let mut attempt_count = 0u32;
    let mut last_overall_status: Option<String> = None;
    let mut first_response_logged = false;
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    while Instant::now() < deadline {
        tokio::select! {
            _ = &mut ctrl_c => {
                debug!("Received Ctrl+C, aborting startup");
                return Err("Startup interrupted by user".to_string());
            }
            _ = tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)) => {
                // Print any new logs line-by-line
                log_streamer.print_new_logs();
                attempt_count += 1;
                let elapsed_ms = start_time.elapsed().as_millis();

                // Check health status
                match status(port).await {
                    Ok(status_response) => {
                        // Log first successful connection to server
                        if !first_response_logged {
                            debug!(
                                "Server responding after {}ms (attempt {}) - now waiting for services",
                                elapsed_ms, attempt_count
                            );
                            first_response_logged = true;
                        }

                        // FAIL FAST: Check if any critical process has permanently failed
                        if status_response.failed {
                            debug!(
                                "Process failure detected after {}ms - frontend: {}, backend: {}",
                                elapsed_ms,
                                status_response.frontend_status,
                                status_response.backend_status
                            );
                            return Err(format!(
                                "Process failed and cannot recover. Frontend: {}, Backend: {}",
                                status_response.frontend_status,
                                status_response.backend_status
                            ));
                        }

                        if status_response.status == "ok" {
                            debug!(
                                "Health check PASSED on attempt {} after {}ms - services ready (frontend: {}, backend: {}, db: {})",
                                attempt_count,
                                elapsed_ms,
                                status_response.frontend_status,
                                status_response.backend_status,
                                status_response.db_status
                            );

                            // Check if DB failed to start (non-critical warning)
                            if status_response.db_status != "healthy" {
                                println!("⚠️  Database not available: local development will work but DB features disabled");
                            }

                            return Ok(());
                        }

                        // Log every attempt - we need to see what's happening
                        let status_str = format!(
                            "status={}, fe={}, be={}, db={}",
                            status_response.status,
                            status_response.frontend_status,
                            status_response.backend_status,
                            status_response.db_status
                        );

                        // Only log if status changed or every 5 seconds to reduce spam
                        let should_log = last_overall_status.as_ref() != Some(&status_str)
                            || attempt_count <= 5
                            || elapsed_ms % 5000 < 250;

                        if should_log {
                            debug!(
                                "Health check attempt {} ({}ms) - {} [waiting for status='ok']",
                                attempt_count, elapsed_ms, status_str
                            );
                        }
                        last_overall_status = Some(status_str);
                    }
                    Err(e) => {
                        // Log connection errors - every attempt for first 5, then every 5s
                        let should_log = attempt_count <= 5 || elapsed_ms % 5000 < 250;
                        if should_log {
                            debug!(
                                "Health check attempt {} ({}ms) - connection failed: {}",
                                attempt_count, elapsed_ms, e
                            );
                        }
                        last_overall_status = None;
                    }
                }
            }
        }
    }

    // Log final state before returning error
    debug!(
        "Health check TIMED OUT after {} attempts ({}ms). Last state: {:?}",
        attempt_count,
        start_time.elapsed().as_millis(),
        last_overall_status
    );

    Err(format!(
        "Dev server failed to become healthy after {}s timeout",
        config.timeout_secs
    ))
}
