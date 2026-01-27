use clap::Args;
use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::bun_binary_path;
use crate::cli::dev::logs::LogsArgs;
use crate::cli::dev::stop::stop_dev_server;
use crate::cli::run_cli_async;
use crate::common::{ensure_dir, spinner, format_elapsed_ms, run_preflight_checks};
use crate::dev::client::{health, stop_with_logs, wait_for_healthy, HealthCheckConfig};
use crate::dev::common::{lock_path, read_lock, remove_lock, write_lock, DevLock, BIND_HOST};
use crate::dev::process::ProcessManager;
use crate::otelcol;
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

    let _ = spawn_server(&app_dir, None, args.skip_credentials_validation).await?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    let port = if let Some(port) = resolve_existing_server(&app_dir).await? {
        println!("✅ Dev server already running at http://localhost:{port}, attaching logs...\n");
        port
    } else {
        spawn_server(&app_dir, None, args.skip_credentials_validation).await?
    };

    // Use the file-based log following (reads from otelcol output)
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
    let is_healthy = health(lock.port).await?;
    if is_healthy {
        Ok(Some(lock.port))
    } else {
        println!("⚠️  Dev server unreachable at http://localhost:{}", lock.port);
        Ok(None)
    }
}

/// Start a dev server for the given app directory.
/// If a server is already running and healthy, returns its port.
/// Otherwise spawns a new server subprocess.
pub async fn start_dev_server(app_dir: &Path) -> Result<u16, String> {
    if let Some(port) = resolve_existing_server(app_dir).await? {
        return Ok(port);
    }
    spawn_server(app_dir, None, false).await
}

/// Path to the startup log file within the .apx directory.
fn startup_log_path(app_dir: &Path) -> PathBuf {
    app_dir.join(".apx/startup.log")
}

/// Read and format startup log contents for error display.
fn read_startup_log(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Run preflight checks and display progress.
async fn run_preflight(app_dir: &Path) -> Result<(), String> {
    println!("🛫 Preflight check started...");
    let preflight_start = Instant::now();
    
    let bun_path = bun_binary_path()?;
    let preflight_spinner = spinner("  Running preflight checks...");
    
    let result = run_preflight_checks(app_dir, &bun_path).await;
    preflight_spinner.finish_and_clear();
    
    match result {
        Ok(preflight) => {
            println!("  ✓ verified project layout ({}ms)", preflight.layout_ms);
            println!("  ✓ uv sync ({}ms)", preflight.uv_sync_ms);
            println!("  ✓ version file ({}ms)", preflight.version_ms);
            if let Some(bun_ms) = preflight.bun_install_ms {
                println!("  ✓ bun install ({}ms)", bun_ms);
            } else {
                println!("  ✓ node_modules (cached)");
            }
            println!("✅ Ready for takeoff! ({})\n", format_elapsed_ms(preflight_start));
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
) -> Result<u16, String> {
    let start_time = Instant::now();
    prepare_app_dir(app_dir)?;
    
    // Run preflight checks (generates _metadata.py, _version.py, installs deps)
    run_preflight(app_dir).await?;
    
    let lock_path = lock_path(app_dir);

    println!("🚀 Starting dev server...");
    
    // Start otelcol for log collection (before subprocess so it's ready to receive logs)
    if let Err(e) = otelcol::ensure_otelcol_running() {
        debug!("Failed to start otelcol: {e}. Logs may not be collected.");
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
    let command = format!(
        "uv run apx dev __internal__run_server --app-dir {} --host {} --port {}{}",
        app_dir.display(),
        BIND_HOST,
        port,
        if skip_credentials_validation { " --skip-credentials-validation" } else { "" }
    );

    // Create startup log file to capture early stderr
    let startup_log = startup_log_path(app_dir);
    let startup_file = File::create(&startup_log)
        .map_err(|err| format!("Failed to create startup log: {err}"))?;

    let mut cmd = Command::new("uv");
    cmd.arg("run")
        .arg("apx")
        .arg("dev")
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
    
    let mut child = cmd
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(startup_file))
        .env("APX_COLLECT_LOGS", "1")
        .env("APX_OTEL_LOGS", "1")
        .spawn()
        .map_err(|err| format!("Failed to start dev server: {err}"))?;

    let health_spinner = spinner("⏳ Waiting for dev server to become healthy...");
    let mut config = HealthCheckConfig::default();
    config.print_waiting = false; // Don't print, we have a spinner instead
    if let Err(e) = wait_for_healthy(port, &config).await {
        health_spinner.finish_and_clear();

        // Try graceful shutdown with log persistence (5 second timeout)
        // This dumps subprocess logs to startup.log before stopping
        debug!("Health checks failed, attempting graceful shutdown with log persistence.");
        let shutdown_result = tokio::time::timeout(
            Duration::from_secs(5),
            stop_with_logs(port, true),
        )
        .await;

        match shutdown_result {
            Ok(Ok(())) => debug!("Graceful shutdown completed."),
            Ok(Err(err)) => debug!("Graceful shutdown failed: {}", err),
            Err(_) => debug!("Graceful shutdown timed out."),
        }

        // Kill any remaining processes (in case graceful shutdown failed or timed out)
        let pid = child.id();
        let _ = ProcessManager::kill_process_tree_async(pid, "dev-server".to_string()).await;
        let _ = child.kill(); // Fallback in case tree kill missed the root

        // Clean up lock file if it exists
        let _ = remove_lock(&lock_path);

        // Read and display startup log on failure (now includes subprocess logs)
        if let Some(log_content) = read_startup_log(&startup_log) {
            eprintln!("\n📋 Startup log:\n{}\n", log_content);
        }

        return Err(e);
    }
    health_spinner.finish_and_clear();

    // Remove startup log on success
    let _ = fs::remove_file(&startup_log);

    let lock = DevLock::new(child.id(), port, command, app_dir);
    write_lock(&lock_path, &lock)?;

    println!("✅ Dev server started at http://localhost:{port} in {}\n", format_elapsed_ms(start_time));
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
        "Port {port} is still in use after {}ms. Another process may be using it.",
        PORT_WAIT_TIMEOUT_MS
    ))
}
