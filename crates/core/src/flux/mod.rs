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

use std::fs;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// Re-export from apx-common crate
pub use apx_common::{
    FLUX_PORT, FluxLock, flux_dir, is_flux_listening, is_running, log_path, read_lock, remove_lock,
    write_lock,
};

// ============================================================================
// Daemon management
// ============================================================================

/// Spawn flux as a detached daemon process using the apx-agent binary.
fn spawn_daemon() -> Result<u32, String> {
    let log_file = log_path()?;

    // Ensure log directory exists
    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create log directory: {e}"))?;
    }

    // Open log file for daemon output
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| format!("Failed to open log file: {e}"))?;

    let log_stderr = log
        .try_clone()
        .map_err(|e| format!("Failed to clone log file handle: {e}"))?;

    // Get the agent binary path (installs if needed)
    let agent_path = crate::agent::ensure_installed()?;

    debug!("Spawning flux daemon: {}", agent_path.display());

    let child = std::process::Command::new(&agent_path)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_stderr)
        .spawn()
        .map_err(|e| format!("Failed to spawn agent: {e}"))?;

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
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!("Flux did not start within {timeout_ms}ms"))
}

/// Start flux daemon.
///
/// Spawns a new flux daemon process if one is not already running.
/// Returns an error if flux cannot be started.
pub fn start() -> Result<(), String> {
    // Create the flux directory if it doesn't exist
    let dir = flux_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create flux directory: {e}"))?;

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
