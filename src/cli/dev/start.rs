use clap::Args;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::cli::run_cli;
use crate::common::{ensure_apx_plugin, ensure_dir};
use crate::dev::client::health;
use crate::dev::common::{
    BIND_HOST, DEFAULT_HOST, DevLock, find_available_port, lock_path, read_lock, write_lock,
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
}

pub fn run(args: StartArgs) -> i32 {
    run_cli(|| run_inner(args))
}

fn run_inner(args: StartArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    ensure_apx_plugin(&app_dir)?;
    ensure_dir(&app_dir.join(".apx"))?;

    let lock_path = lock_path(&app_dir);
    if lock_path.exists() {
        let lock = read_lock(&lock_path)?;
        let is_healthy = health(lock.port)?;
        let status = if is_healthy { "healthy" } else { "unreachable" };
        println!(
            "Dev server already at http://{DEFAULT_HOST}:{port}, status: {status}",
            port = lock.port
        );
        return Ok(());
    }

    println!("No lock file found, starting dev server");
    let port = find_available_port(BIND_HOST)?;
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
        .arg(&app_dir)
        .arg("--host")
        .arg(BIND_HOST)
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&app_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

    let lock = DevLock::new(child.id(), port, command, &app_dir);
    write_lock(&lock_path, &lock)?;

    println!("Dev server started at http://{DEFAULT_HOST}:{port}");
    Ok(())
}
