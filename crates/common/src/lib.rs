//! Shared types and utilities for APX flux system
//!
//! This crate contains shared functionality used by both the main `apx` CLI
//! and the standalone `apx-agent` binary.

pub mod storage;

use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

// Re-export commonly used types
pub use storage::{LogRecord, Storage, db_path, flux_dir};

/// Flux port for OTLP HTTP receiver
pub const FLUX_PORT: u16 = 11111;

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

    let contents =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read flux lock file: {e}"))?;

    let lock: FluxLock = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse flux lock file: {e}"))?;

    Ok(Some(lock))
}

/// Write the lock file.
pub fn write_lock(lock: &FluxLock) -> Result<(), String> {
    let path = lock_path()?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create lock directory: {e}"))?;
    }

    let contents =
        serde_json::to_string_pretty(lock).map_err(|e| format!("Failed to serialize lock: {e}"))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write flux lock file: {e}"))
}

/// Remove the lock file.
pub fn remove_lock() -> Result<(), String> {
    let path = lock_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("Failed to remove flux lock file: {e}"))?;
    }
    Ok(())
}

/// Check if flux is accepting connections at the given port.
pub fn is_flux_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Check if flux is currently running by testing TCP connectivity.
pub fn is_running() -> bool {
    is_flux_listening(FLUX_PORT)
}
