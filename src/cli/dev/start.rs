use clap::Args;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::cli::dev::logs::stream_logs;
use crate::cli::dev::stop::stop_dev_server;
use crate::cli::run_cli_async;
use crate::common::ensure_dir;
use crate::dev::client::{health, logs, wait_for_healthy, HealthCheckConfig};
use crate::dev::common::{
    find_available_port, lock_path, read_lock, write_lock,
    DevLock, BIND_HOST, CLIENT_HOST,
};

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
        println!(
            "Dev server already at http://{CLIENT_HOST}:{port}, status: healthy",
            port = port
        );
        return Ok(());
    }

    let _ = spawn_server(&app_dir, None).await?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    let port = if let Some(port) = resolve_existing_server(&app_dir).await? {
        println!(
            "Dev server already at http://{CLIENT_HOST}:{port}, attaching logs",
            port = port
        );
        port
    } else {
        spawn_server(&app_dir, None).await?
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
        println!(
            "Dev server already at http://{CLIENT_HOST}:{port}, status: unreachable",
            port = lock.port
        );
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
    spawn_server(app_dir, None).await
}

/// Spawn a new dev server subprocess (does not check for existing server).
pub(crate) async fn spawn_server(
    app_dir: &Path,
    preferred_port: Option<u16>,
) -> Result<u16, String> {
    prepare_app_dir(app_dir)?;
    let lock_path = lock_path(app_dir);

    println!("Starting dev server...");
    let port = resolve_port(preferred_port).await?;
    let command = format!(
        "uv run apx dev __internal__run_server --app-dir {} --host {} --port {}",
        app_dir.display(),
        BIND_HOST,
        port
    );

    let mut child = Command::new("uv")
        .arg("run")
        .arg("apx")
        .arg("dev")
        .arg("__internal__run_server")
        .arg("--app-dir")
        .arg(app_dir)
        .arg("--host")
        .arg(BIND_HOST)
        .arg("--port")
        .arg(port.to_string())
        .current_dir(app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("APX_COLLECT_LOGS", "1")
        .spawn()
        .map_err(|err| format!("Failed to start dev server: {err}"))?;

    if let Err(e) = wait_for_healthy(port, &HealthCheckConfig::default()).await {
        let _ = child.kill();
        return Err(e);
    }

    let lock = DevLock::new(child.id(), port, command, app_dir);
    write_lock(&lock_path, &lock)?;

    println!("Dev server started at http://{CLIENT_HOST}:{port}");
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
                println!("Waiting for port {port} to become available...");
            }
            tokio::time::sleep(Duration::from_millis(PORT_WAIT_INTERVAL_MS)).await;
        }
        println!("Port {port} still in use, finding alternative...");
    }
    find_available_port(BIND_HOST)
}
