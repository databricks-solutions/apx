//! Native Rust OTEL Log Collector
//!
//! This module provides a native OpenTelemetry log collector that replaces
//! the external otelcol binary. It stores logs in SQLite, runs as a detached
//! daemon on port 11111, and supports both HTTP/JSON and HTTP/Protobuf OTLP protocols.
//!
//! ## Usage
//!
//! ```ignore
//! use apx::collector;
//!
//! // Ensure the collector is running (starts if not)
//! collector::ensure_running()?;
//!
//! // Check if the collector is running
//! if collector::is_running() {
//!     println!("Collector is running");
//! }
//!
//! // Stop the collector
//! collector::stop()?;
//! ```

use std::fs;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// Re-export from apx-common crate
pub use apx_common::{
    COLLECTOR_PORT, CollectorLock, collector_dir, is_collector_listening, is_running, log_path,
    read_lock, remove_lock, write_lock,
};

// ============================================================================
// Daemon management
// ============================================================================

/// Spawn the collector as a detached daemon process using the apx-agent binary.
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
    let agent_path = crate::tracing_binary::ensure_installed()?;

    debug!("Spawning collector daemon: {}", agent_path.display());

    let child = std::process::Command::new(&agent_path)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_stderr)
        .spawn()
        .map_err(|e| format!("Failed to spawn agent: {e}"))?;

    let pid = child.id();
    info!("Spawned collector daemon with pid={}", pid);

    Ok(pid)
}

/// Wait for the collector to start accepting connections.
fn wait_for_ready(timeout_ms: u64) -> Result<(), String> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    while start.elapsed() < timeout {
        let addr =
            std::net::SocketAddr::from((apx_common::hosts::CLIENT_HOST_OCTETS, COLLECTOR_PORT));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!("Collector did not start within {timeout_ms}ms"))
}

/// Start the collector daemon.
///
/// Spawns a new collector daemon process if one is not already running.
/// Returns an error if the collector cannot be started.
pub fn start() -> Result<(), String> {
    // Create the collector directory if it doesn't exist
    let dir = collector_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create collector directory: {e}"))?;

    // Check if already running via lock file
    if let Some(lock) = read_lock()? {
        if is_collector_listening(lock.port) {
            debug!(
                "Collector already running (pid={}, port={})",
                lock.pid, lock.port
            );
            return Ok(());
        }

        // Stale lock - clean up
        debug!("Stale collector lock found, cleaning up");
        remove_lock()?;
    }

    // Check if something else is using the port
    if is_collector_listening(COLLECTOR_PORT) {
        warn!(
            "Port {} is in use but no valid lock file found. Assuming collector is running.",
            COLLECTOR_PORT
        );
        return Ok(());
    }

    // Start the daemon
    info!("Starting collector daemon on port {}", COLLECTOR_PORT);
    let pid = spawn_daemon()?;

    // Wait for it to be ready
    wait_for_ready(5000)?;

    // Write lock file
    let lock = CollectorLock::new(pid);
    write_lock(&lock)?;

    info!("Collector daemon started successfully (pid={})", pid);
    Ok(())
}

/// Ensure the collector is running, starting it if necessary.
///
/// This is the main API for callers like `apx dev start` that need to ensure
/// the collector is running before proceeding. Also checks that the running daemon
/// matches the current apx version — restarts on mismatch.
pub fn ensure_running() -> Result<(), String> {
    if is_running() {
        // Check version from lock file
        if let Some(lock) = read_lock()? {
            if lock.version.as_deref() == Some(apx_common::VERSION) {
                debug!("Collector is already running (version matches)");
                return Ok(());
            }
            // Version mismatch or old lock without version — restart
            info!(
                "Collector version mismatch (running: {:?}, expected: {}), restarting",
                lock.version,
                apx_common::VERSION
            );
            stop()?;
            // Fall through to start()
        } else {
            debug!("Collector is already running (no lock file to check version)");
            return Ok(());
        }
    }
    start()
}

/// Stop the collector daemon.
///
/// Stops the running collector daemon and removes the lock file.
pub fn stop() -> Result<(), String> {
    let Some(lock) = read_lock()? else {
        debug!("Collector is not running (no lock file)");
        return Ok(());
    };

    if !is_collector_listening(lock.port) {
        debug!("Collector is not listening, cleaning up stale lock");
        remove_lock()?;
        return Ok(());
    }

    info!("Stopping collector daemon (pid={})", lock.pid);

    // Kill the process tree
    if let Err(e) = crate::dev::common::kill_process_tree(lock.pid, "collector-daemon") {
        warn!("Failed to kill collector process tree: {}", e);
    }

    // Wait a bit for the process to exit
    std::thread::sleep(Duration::from_millis(500));

    remove_lock()?;
    info!("Collector daemon stopped");
    Ok(())
}
