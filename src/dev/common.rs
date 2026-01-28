use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::ensure_dir;

/// Shutdown signal type for the dev server.
/// Used as a single authority for coordinating shutdown across all components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// Stop the entire dev server
    Stop,
}

pub const DEV_LOCK_DIR: &str = ".apx";
pub const DEV_LOCK_FILE: &str = "dev.lock";
pub const FRONTEND_PORT_START: u16 = 5000;
pub const FRONTEND_PORT_END: u16 = 5999;
pub const BACKEND_PORT_START: u16 = 8000;
pub const BACKEND_PORT_END: u16 = 8999;
pub const DEV_PORT_START: u16 = 9000;
pub const DB_PORT_START: u16 = 4000;
pub const DB_PORT_END: u16 = 4999;
/// Host used for binding the server (all interfaces)
pub const BIND_HOST: &str = "0.0.0.0";
/// Host used for client connections (localhost)
pub const CLIENT_HOST: &str = "127.0.0.1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevLock {
    pub pid: u32,
    pub started_at: String,
    pub port: u16,
    pub command: String,
    pub app_dir: String,
}

impl DevLock {
    pub fn new(pid: u32, port: u16, command: String, app_dir: &Path) -> Self {
        let started_at: DateTime<Utc> = Utc::now();
        Self {
            pid,
            started_at: started_at.to_rfc3339(),
            port,
            command,
            app_dir: app_dir.display().to_string(),
        }
    }
}

pub fn lock_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(DEV_LOCK_DIR)
}

pub fn lock_path(app_dir: &Path) -> PathBuf {
    lock_dir(app_dir).join(DEV_LOCK_FILE)
}

pub fn read_lock(path: &Path) -> Result<DevLock, String> {
    let contents =
        fs::read_to_string(path).map_err(|err| format!("Failed to read lockfile: {err}"))?;
    serde_json::from_str(&contents).map_err(|err| format!("Invalid lockfile JSON: {err}"))
}

pub fn write_lock(path: &Path, lock: &DevLock) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let contents =
        serde_json::to_string_pretty(lock).map_err(|err| format!("Lockfile JSON error: {err}"))?;
    fs::write(path, contents).map_err(|err| format!("Failed to write lockfile: {err}"))
}

pub fn remove_lock(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path).map_err(|err| format!("Failed to remove lockfile: {err}"))?;
    }
    Ok(())
}

/// Find an available port in the given range, starting from a random offset.
/// This reduces collision probability when multiple processes are looking for ports
/// simultaneously.
pub fn find_random_port_in_range(host: &str, start: u16, end: u16) -> Result<u16, String> {
    use rand::Rng;
    use std::net::TcpListener;
    let range_size = (end - start + 1) as usize;
    let offset = rand::thread_rng().gen_range(0..range_size);

    // Try from random offset, then wrap around
    for i in 0..range_size {
        let port = start + ((offset + i) % range_size) as u16;
        if TcpListener::bind((host, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(format!("No available ports in range {start}-{end}"))
}
