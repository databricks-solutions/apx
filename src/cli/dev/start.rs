use clap::Args;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::cli::dev::logs::stream_logs;
use crate::cli::dev::stop::stop_server_inner;
use crate::cli::run_cli;
use crate::common::{ensure_dir, sync_apx_plugin_from_package};
use crate::dev::client::{health, health_async, logs_async};
use crate::dev::common::{
    BIND_HOST, CLIENT_HOST, DevLock, find_available_port, lock_path, read_lock, write_lock,
};

const HEALTH_RETRY_COUNT: u32 = 20;
const HEALTH_RETRY_DELAY_MS: u64 = 200;

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

pub fn run(args: StartArgs) -> i32 {
    if args.attached {
        run_cli(|| {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|err| format!("Failed to create tokio runtime: {err}"))?;
            runtime.block_on(run_attached(args))
        })
    } else {
        run_cli(|| run_inner(args))
    }
}

fn run_inner(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    if let Some(port) = resolve_existing_server(&app_dir)? {
        println!(
            "Dev server already at http://{CLIENT_HOST}:{port}, status: healthy",
            port = port
        );
        return Ok(());
    }

    let _ = start_server(&app_dir, None)?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(&args);
    let port = if let Some(port) = resolve_existing_server_async(&app_dir).await? {
        println!(
            "Dev server already at http://{CLIENT_HOST}:{port}, attaching logs",
            port = port
        );
        port
    } else {
        start_server_async(&app_dir, None).await?
    };

    let response = logs_async(port, None, true).await?;
    let _ = stream_logs(response, true).await;

    stop_server_inner(&app_dir)
}

fn resolve_app_dir(args: &StartArgs) -> PathBuf {
    args.app_path
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn resolve_existing_server(app_dir: &PathBuf) -> Result<Option<u16>, String> {
    let lock_path = lock_path(app_dir);
    if !lock_path.exists() {
        return Ok(None);
    }

    let lock = read_lock(&lock_path)?;
    let is_healthy = health(lock.port)?;
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

async fn resolve_existing_server_async(app_dir: &PathBuf) -> Result<Option<u16>, String> {
    let lock_path = lock_path(app_dir);
    if !lock_path.exists() {
        return Ok(None);
    }

    let lock = read_lock(&lock_path)?;
    let is_healthy = health_async(lock.port).await?;
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

pub(crate) fn start_server(app_dir: &PathBuf, preferred_port: Option<u16>) -> Result<u16, String> {
    sync_apx_plugin_from_package(app_dir)?;
    ensure_dir(&app_dir.join(".apx"))?;

    let lock_path = lock_path(app_dir);
    println!("No lock file found, starting dev server");
    let port = resolve_port(preferred_port)?;
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

    println!("Waiting for dev server to become healthy");
    let mut healthy = false;
    for _ in 0..HEALTH_RETRY_COUNT {
        match health(port) {
            Ok(true) => {
                healthy = true;
                break;
            }
            Ok(false) | Err(_) => {
                sleep(Duration::from_millis(HEALTH_RETRY_DELAY_MS));
            }
        }
    }

    if !healthy {
        let _ = child.kill();
        return Err(format!(
            "Dev server failed to become healthy after {HEALTH_RETRY_COUNT} retries",
            HEALTH_RETRY_COUNT = HEALTH_RETRY_COUNT
        ));
    }

    let lock = DevLock::new(child.id(), port, command, app_dir);
    write_lock(&lock_path, &lock)?;

    println!("Dev server started at http://{CLIENT_HOST}:{port}");
    Ok(port)
}

async fn start_server_async(app_dir: &PathBuf, preferred_port: Option<u16>) -> Result<u16, String> {
    sync_apx_plugin_from_package(app_dir)?;
    ensure_dir(&app_dir.join(".apx"))?;

    let lock_path = lock_path(app_dir);
    println!("No lock file found, starting dev server");
    let port = resolve_port(preferred_port)?;
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

    println!("Waiting for dev server to become healthy");
    let mut healthy = false;
    for _ in 0..HEALTH_RETRY_COUNT {
        match health_async(port).await {
            Ok(true) => {
                healthy = true;
                break;
            }
            Ok(false) | Err(_) => {
                tokio::time::sleep(Duration::from_millis(HEALTH_RETRY_DELAY_MS)).await;
            }
        }
    }

    if !healthy {
        let _ = child.kill();
        return Err(format!(
            "Dev server failed to become healthy after {HEALTH_RETRY_COUNT} retries",
            HEALTH_RETRY_COUNT = HEALTH_RETRY_COUNT
        ));
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

fn resolve_port(preferred_port: Option<u16>) -> Result<u16, String> {
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
            sleep(Duration::from_millis(PORT_WAIT_INTERVAL_MS));
        }
        println!("Port {port} still in use, finding alternative...");
    }
    find_available_port(BIND_HOST)
}
