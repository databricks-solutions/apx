use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use sysinfo::{Pid, Signal, System};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{debug, warn};

use crate::common::ensure_dir;

// ---------------------------------------------------------------------------
// Health probe utilities (shared by process.rs and backend.rs)
// ---------------------------------------------------------------------------

/// Server-side probe timeout in seconds.
/// Must be strictly less than the client-side per-request timeout (DEFAULT_TIMEOUT_SECS in client.rs)
/// to avoid a race where both timeouts fire simultaneously, causing every poll cycle to fail.
const PROBE_TIMEOUT_SECS: u64 = 1;

/// Shared HTTP client for health probes.
/// Reused across all health checks to avoid creating a new client per probe.
pub(crate) static HEALTH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(2)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Result of an HTTP health probe against a backend/frontend service.
#[allow(dead_code)]
pub(crate) enum ProbeResult {
    /// Service responded with the given HTTP status code — it is up.
    Responded(u16),
    /// Connection or timeout error — service is not ready yet.
    Failed(String),
}

/// Probe a service by making an HTTP GET request to its root path.
/// Any HTTP response (regardless of status code) means the server is up.
/// Only connection/timeout failures indicate the server isn't ready yet.
pub(crate) async fn http_health_probe(host: &str, port: u16) -> ProbeResult {
    let url = format!("http://{host}:{port}/");
    let start = std::time::Instant::now();
    match HEALTH_CLIENT.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let elapsed_ms = start.elapsed().as_millis();
            if status == 200 {
                debug!(url = %url, status, elapsed_ms, "Health probe OK");
            } else {
                warn!(url = %url, status, elapsed_ms, "Health probe returned non-200");
            }
            ProbeResult::Responded(status)
        }
        Err(err) => {
            let elapsed_ms = start.elapsed().as_millis();
            debug!(url = %url, error = %err, elapsed_ms, "Health probe failed");
            ProbeResult::Failed(err.to_string())
        }
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevLock {
    pub pid: u32,
    pub started_at: String,
    pub port: u16,
    pub command: String,
    pub app_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl DevLock {
    pub fn new(pid: u32, port: u16, command: String, app_dir: &Path, token: String) -> Self {
        let started_at: DateTime<Utc> = Utc::now();
        Self {
            pid,
            started_at: started_at.to_rfc3339(),
            port,
            command,
            app_dir: app_dir.display().to_string(),
            token: Some(token),
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

/// Check if a process with the given PID is still running.
/// Uses sysinfo crate for cross-platform compatibility (Linux, macOS, Windows).
pub fn is_process_running(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid)).is_some()
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

// ---------------------------------------------------------------------------
// DevProcess trait and shared process management utilities
// ---------------------------------------------------------------------------

/// Shared lifecycle contract for a managed dev subprocess.
/// Implementors hold an `Arc<Mutex<Option<Child>>>` internally.
pub(crate) trait DevProcess: Send + Sync {
    /// Access the child handle for shutdown orchestration.
    fn child_handle(&self) -> &Arc<Mutex<Option<Child>>>;

    /// Human-readable label for log messages ("backend", "db").
    fn label(&self) -> &'static str;

    /// Report current process status as a static label.
    async fn status(&self) -> &'static str;
}

/// Kill a child process tree immediately (used for restart operations).
/// Shared by `ProcessManager::stop()` and `Backend::stop_current()`.
pub(crate) async fn stop_child_tree(name: &str, child: &Arc<Mutex<Option<Child>>>) {
    let mut guard = child.lock().await;
    if let Some(mut child) = guard.take() {
        let pid = child.id();
        if let Some(pid) = pid {
            if let Err(err) = kill_process_tree_async(pid, name.to_string()).await {
                warn!(error = %err, process = name, pid, "Failed to kill process tree.");
            }
        } else {
            warn!(process = name, "Missing PID for child process.");
        }
        match timeout(Duration::from_secs(2), child.wait()).await {
            Ok(Ok(status)) => debug!(process = name, ?status, "Child process exited."),
            Ok(Err(err)) => {
                warn!(error = %err, process = name, "Failed to wait for child process.")
            }
            Err(_) => warn!(
                process = name,
                "Timed out waiting for child process to exit."
            ),
        }
    } else {
        debug!(process = name, "No child process to stop.");
    }
}

/// Kill a process tree. This is a blocking operation that should be called
/// from a blocking context or wrapped in `spawn_blocking`.
pub(crate) fn kill_process_tree(pid: u32, label: &str) -> Result<(), String> {
    let root_pid = Pid::from_u32(pid);
    let mut sys = System::new_all();
    sys.refresh_all();
    let root_process = sys
        .process(root_pid)
        .ok_or_else(|| format!("{label} process {pid} not found"))?;
    let root_start_time = root_process.start_time();
    let parents = build_parent_map(&sys);
    debug!(
        pid = ?root_pid,
        root_start_time,
        process = label,
        "Killing process tree."
    );
    kill_tree_with_guard(&sys, &parents, root_pid, root_start_time, label);
    Ok(())
}

/// Async wrapper for `kill_process_tree` that runs on a blocking thread.
pub(crate) async fn kill_process_tree_async(pid: u32, label: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || kill_process_tree(pid, &label))
        .await
        .map_err(|err| format!("Failed to spawn blocking task: {err}"))?
}

pub(crate) fn build_parent_map(sys: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut parents: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in sys.processes() {
        if let Some(parent) = process.parent() {
            parents.entry(parent).or_default().push(*pid);
        }
    }
    parents
}

fn kill_tree_with_guard(
    sys: &System,
    parents: &HashMap<Pid, Vec<Pid>>,
    pid: Pid,
    root_start_time: u64,
    label: &str,
) {
    if let Some(children) = parents.get(&pid) {
        for child_pid in children {
            kill_tree_with_guard(sys, parents, *child_pid, root_start_time, label);
        }
    }

    if let Some(process) = sys.process(pid) {
        let process_start_time = process.start_time();
        if process_start_time < root_start_time {
            debug!(
                pid = ?pid,
                process_start_time,
                root_start_time,
                process = label,
                "Skipping process because it predates the root."
            );
            return;
        }
        let name = process.name();
        match process.kill_with(Signal::Kill) {
            Some(true) => {
                debug!(pid = ?pid, process_name = ?name, process = label, "Killed process.");
            }
            Some(false) => {
                warn!(pid = ?pid, process_name = ?name, process = label, "kill_with(SIGKILL) returned false — process may require elevated privileges.");
            }
            None => {
                warn!(pid = ?pid, process_name = ?name, process = label, "kill_with(SIGKILL) not supported on this platform for this process.");
            }
        }
    }
}
