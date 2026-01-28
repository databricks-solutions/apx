//! Flux: Native Rust OTEL Collector
//!
//! This module provides a native OpenTelemetry log collector that replaces
//! the external otelcol binary. It stores logs in SQLite, runs as a detached
//! daemon on port 11111, and supports both HTTP/JSON and HTTP/Protobuf OTLP protocols.
//!
//! ## Usage
//!
//! ```ignore
//! use apx::flux;
//!
//! // Ensure flux is running (starts if not)
//! flux::ensure_running()?;
//!
//! // Check if flux is running
//! if flux::is_running() {
//!     println!("Flux is running");
//! }
//!
//! // Stop flux
//! flux::stop()?;
//! ```

mod server;
pub mod storage;

use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

pub use server::{run_server, FLUX_PORT};
pub use storage::{db_path, flux_dir, LogRecord, Storage};

// ============================================================================
// Lock file management
// ============================================================================

/// Lock filename
const LOCK_FILENAME: &str = "agent.lock";

/// Log filename for daemon output
const LOG_FILENAME: &str = "agent.log";

/// Lock file contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluxLock {
    pub pid: u32,
    pub port: u16,
    pub started_at: i64,
}

impl FluxLock {
    /// Create a new lock for the current process.
    pub fn new(pid: u32) -> Self {
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            pid,
            port: FLUX_PORT,
            started_at,
        }
    }
}

/// Get the lock file path (~/.apx/logs/agent.lock).
pub fn lock_path() -> Result<PathBuf, String> {
    Ok(flux_dir()?.join(LOCK_FILENAME))
}

/// Get the daemon log file path (~/.apx/logs/agent.log).
pub fn log_path() -> Result<PathBuf, String> {
    Ok(flux_dir()?.join(LOG_FILENAME))
}

/// Read the lock file if it exists.
pub fn read_lock() -> Result<Option<FluxLock>, String> {
    let path = lock_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read flux lock file: {}", e))?;

    let lock: FluxLock = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse flux lock file: {}", e))?;

    Ok(Some(lock))
}

/// Write the lock file.
pub fn write_lock(lock: &FluxLock) -> Result<(), String> {
    let path = lock_path()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create lock directory: {}", e))?;
    }

    let contents = serde_json::to_string_pretty(lock)
        .map_err(|e| format!("Failed to serialize lock: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write flux lock file: {}", e))
}

/// Remove the lock file.
pub fn remove_lock() -> Result<(), String> {
    let path = lock_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("Failed to remove flux lock file: {}", e))?;
    }
    Ok(())
}

// ============================================================================
// Status checking
// ============================================================================

/// Check if flux is accepting connections at the given port.
pub fn is_flux_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Check if flux is currently running by testing TCP connectivity.
pub fn is_running() -> bool {
    is_flux_listening(FLUX_PORT)
}

// ============================================================================
// Daemon management
// ============================================================================

/// Spawn flux as a detached daemon process using the current apx executable.
fn spawn_daemon() -> Result<u32, String> {
    let log_file = log_path()?;

    // Ensure log directory exists
    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create log directory: {}", e))?;
    }

    // Open log file for daemon output
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| format!("Failed to open log file: {}", e))?;

    let log_stderr = log
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {}", e))?;

    // Get the apx command to avoid spawning via `uv run`
    // which would create an extra process and lock the uv cache
    let apx_cmd = crate::common::ApxCommand::new()?;

    debug!("Spawning flux daemon: {} flux __run", apx_cmd.display());

    let mut cmd = apx_cmd.command();
    let child = cmd
        .args(["flux", "__run"])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_stderr)
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon: {}", e))?;

    let pid = child.id();
    info!("Spawned flux daemon with pid={}", pid);

    Ok(pid)
}

/// Wait for flux to start accepting connections.
fn wait_for_ready(timeout_ms: u64) -> Result<(), String> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    while start.elapsed() < timeout {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], FLUX_PORT));
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!("Flux did not start within {}ms", timeout_ms))
}

/// Start flux daemon.
///
/// Spawns a new flux daemon process if one is not already running.
/// Returns an error if flux cannot be started.
pub fn start() -> Result<(), String> {
    // Create the flux directory if it doesn't exist
    let dir = flux_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create flux directory: {}", e))?;

    // Check if already running via lock file
    if let Some(lock) = read_lock()? {
        if is_flux_listening(lock.port) {
            debug!(
                "Flux already running (pid={}, port={})",
                lock.pid, lock.port
            );
            return Ok(());
        }

        // Stale lock - clean up
        debug!("Stale flux lock found, cleaning up");
        remove_lock()?;
    }

    // Check if something else is using the port
    if is_flux_listening(FLUX_PORT) {
        warn!(
            "Port {} is in use but no valid lock file found. Assuming flux is running.",
            FLUX_PORT
        );
        return Ok(());
    }

    // Start the daemon
    info!("Starting flux daemon on port {}", FLUX_PORT);
    let pid = spawn_daemon()?;

    // Wait for it to be ready
    wait_for_ready(5000)?;

    // Write lock file
    let lock = FluxLock::new(pid);
    write_lock(&lock)?;

    info!("Flux daemon started successfully (pid={})", pid);
    Ok(())
}

/// Ensure flux is running, starting it if necessary.
///
/// This is the main API for callers like `apx dev start` that need to ensure
/// flux is running before proceeding.
pub fn ensure_running() -> Result<(), String> {
    if is_running() {
        debug!("Flux is already running");
        return Ok(());
    }
    start()
}

/// Stop flux daemon.
///
/// Stops the running flux daemon and removes the lock file.
pub fn stop() -> Result<(), String> {
    use crate::dev::process::ProcessManager;

    let lock = match read_lock()? {
        Some(l) => l,
        None => {
            debug!("Flux is not running (no lock file)");
            return Ok(());
        }
    };

    if !is_flux_listening(lock.port) {
        debug!("Flux is not listening, cleaning up stale lock");
        remove_lock()?;
        return Ok(());
    }

    info!("Stopping flux daemon (pid={})", lock.pid);

    // Use ProcessManager to kill the process tree
    if let Err(e) = ProcessManager::kill_process_tree(lock.pid, "flux-daemon") {
        warn!("Failed to kill flux process tree: {}", e);
    }

    // Wait a bit for the process to exit
    std::thread::sleep(Duration::from_millis(500));

    remove_lock()?;
    info!("Flux daemon stopped");
    Ok(())
}
