use clap::Args;
use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli::dev::logs::stream_logs;
use crate::cli::dev::stop::stop_dev_server;
use crate::cli::run_cli_async;
use crate::common::{ensure_dir, spinner, format_elapsed_ms};
use crate::dev::client::{health, logs, wait_for_healthy, HealthCheckConfig};
use crate::dev::common::{
    find_available_port, lock_path, read_lock, remove_lock, write_lock,
    DevLock, BIND_HOST,
};
use crate::dev::process::ProcessManager;

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

    let response = logs(port, None, true).await?;
    let _ = stream_logs(response, true).await;

    stop_dev_server(&app_dir).await
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

/// Spawn a new dev server subprocess (does not check for existing server).
pub(crate) async fn spawn_server(
    app_dir: &Path,
    preferred_port: Option<u16>,
    skip_credentials_validation: bool,
) -> Result<u16, String> {
    let start_time = Instant::now();
    prepare_app_dir(app_dir)?;
    let lock_path = lock_path(app_dir);

    println!("🚀 Starting dev server...");
    let port = resolve_port(preferred_port).await?;
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
        .spawn()
        .map_err(|err| format!("Failed to start dev server: {err}"))?;

    let health_spinner = spinner("⏳ Waiting for dev server to become healthy...");
    let mut config = HealthCheckConfig::default();
    config.print_waiting = false; // Don't print, we have a spinner instead
    if let Err(e) = wait_for_healthy(port, &config).await {
        health_spinner.finish_and_clear();
        
        // Kill the entire process tree to avoid hanging processes
        let pid = child.id();
        let _ = ProcessManager::kill_process_tree_async(pid, "dev-server".to_string()).await;
        let _ = child.kill(); // Fallback in case tree kill missed the root
        
        // Clean up lock file if it exists
        let _ = remove_lock(&lock_path);
        
        // Read and display startup log on failure
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

/// Maximum time to wait for a preferred port to become available (in ms).
const PORT_WAIT_TIMEOUT_MS: u64 = 2000;
/// Interval between port availability checks (in ms).
const PORT_WAIT_INTERVAL_MS: u64 = 100;

async fn resolve_port(preferred_port: Option<u16>) -> Result<u16, String> {
    if let Some(port) = preferred_port {
        // Try the preferred port with retries (in case it's still being released)
        let max_attempts = PORT_WAIT_TIMEOUT_MS / PORT_WAIT_INTERVAL_MS;
        for attempt in 0..max_attempts {
            if TcpListener::bind((BIND_HOST, port)).is_ok() {
                return Ok(port);
            }
            if attempt == 0 {
                println!("⏳ Waiting for port {port} to become available...");
            }
            tokio::time::sleep(Duration::from_millis(PORT_WAIT_INTERVAL_MS)).await;
        }
        println!("⚠️  Port {port} still in use, finding alternative...");
    }
    find_available_port(BIND_HOST)
}
