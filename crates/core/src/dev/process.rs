//! Process management for APX dev server.
//!
//! Orchestrates frontend, backend, and database processes.
//! Individual lifecycles are delegated to [`Frontend`], [`Backend`], and [`EmbeddedDb`].
// Runs inside the dev server process (in-process for attached mode,
// child process for detached mode). Never in the MCP server process
// — stdout output here is safe.
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use sysinfo::{Pid, Signal, System};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{debug, warn};

use crate::common::read_project_metadata;
use crate::dev::backend::{Backend, BackendConfig};
use crate::dev::common::{DevProcess, build_parent_map};
use crate::dev::embedded_db::EmbeddedDb;
use crate::dev::frontend::{Frontend, FrontendConfig};
use crate::dotenv::DotenvFile;

/// Manages the lifecycle of dev server child processes (backend, frontend, db).
#[derive(Debug)]
pub struct ProcessManager {
    frontend: Option<Arc<Frontend>>,
    backend: Arc<Backend>,
    db: Arc<OnceLock<EmbeddedDb>>,
    backend_port: u16,
    db_port: u16,
    dev_server_port: u16,
    host: String,
    app_dir: PathBuf,
    app_slug: String,
}

impl ProcessManager {
    /// Create a new ProcessManager without spawning processes.
    /// Call `start_processes()` to spawn processes in the background.
    pub fn new(
        app_dir: &Path,
        host: &str,
        dev_server_port: u16,
        backend_port: u16,
        frontend_port: Option<u16>,
        db_port: u16,
        dev_token: String,
    ) -> Result<Self, String> {
        // Note: Preflight checks (metadata, uv sync, bun install) are done client-side in start.rs
        let metadata = read_project_metadata(app_dir)?;
        let has_ui = metadata.has_ui();

        let dotenv = DotenvFile::read(&app_dir.join(".env"))?;
        let dotenv_vars = Arc::new(Mutex::new(dotenv.get_vars()));
        let app_slug = metadata.app_slug.clone();
        let app_entrypoint = metadata.app_entrypoint.clone();
        let dev_config = metadata.dev_config;

        let app_dir = app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.to_path_buf());

        let db = Arc::new(OnceLock::new());

        // Frontend is only created when the project has a UI and a port is assigned
        let frontend = if has_ui {
            frontend_port.map(|port| {
                Arc::new(Frontend::new(FrontendConfig {
                    app_dir: app_dir.clone(),
                    app_slug: app_slug.clone(),
                    host: host.to_string(),
                    backend_port,
                    frontend_port: port,
                    db_port,
                    dev_server_port,
                    dev_token: dev_token.clone(),
                }))
            })
        } else {
            None
        };

        let backend = Arc::new(Backend::new(BackendConfig {
            app_dir: app_dir.clone(),
            app_slug: app_slug.clone(),
            app_entrypoint,
            host: host.to_string(),
            backend_port,
            frontend_port,
            db_port,
            dev_server_port,
            dev_token,
            dev_config,
            dotenv_vars,
            db: Arc::clone(&db),
        }));

        debug!(
            app_dir = %app_dir.display(),
            host = %host,
            dev_server_port,
            backend_port,
            ?frontend_port,
            db_port,
            has_ui,
            "Creating ProcessManager"
        );

        Ok(Self {
            frontend,
            backend,
            db,
            backend_port,
            db_port,
            dev_server_port,
            host: host.to_string(),
            app_dir,
            app_slug,
        })
    }

    /// Spawn processes in background (DB → Frontend → Backend).
    /// DB is non-critical - failures are logged but don't block other processes.
    /// This method spawns a background task and returns immediately.
    pub fn start_processes(self: &Arc<Self>) {
        let pm = Arc::clone(self);
        tokio::spawn(async move {
            // 1. DB (non-critical) - warn on failure but continue
            debug!("Starting embedded database process...");
            match EmbeddedDb::start(&pm.app_dir, &pm.host, pm.db_port, &pm.app_slug).await {
                Ok(embedded_db) => {
                    let _ = pm.db.set(embedded_db);
                    debug!("Embedded database started successfully");
                }
                Err(e) => {
                    warn!(
                        "Failed to start embedded database: {}. Continuing without DB.",
                        e
                    );
                }
            }

            // 2. Frontend (critical, but only if project has UI)
            if let Some(ref frontend) = pm.frontend {
                debug!("Starting frontend process...");
                if let Err(e) = frontend.spawn().await {
                    warn!("Failed to start frontend: {}", e);
                    return; // Critical failure
                }
                debug!("Frontend started successfully");
            } else {
                debug!("Skipping frontend (backend-only project)");
            }

            // 3. Uvicorn (critical)
            debug!("Starting uvicorn backend process...");
            if let Err(e) = pm.backend.spawn().await {
                warn!("Failed to start backend: {}", e);
                return; // Critical failure
            }
            debug!("Uvicorn backend started successfully");

            debug!("All processes spawned, starting file watcher");
            pm.backend.start_file_watcher();
        });
    }

    /// Return the dev authentication token.
    pub fn dev_token(&self) -> &str {
        self.backend.dev_token()
    }

    /// Stop all managed processes using a phased shutdown approach:
    /// 1. Send SIGTERM to allow graceful shutdown
    /// 2. Wait briefly for processes to exit
    /// 3. Force kill any remaining processes
    pub async fn stop(&self) {
        debug!(
            host = %self.host,
            has_frontend = self.frontend.is_some(),
            backend_port = self.backend_port,
            db_port = self.db_port,
            dev_server_port = self.dev_server_port,
            "Stopping dev processes with phased shutdown."
        );

        let backend_child = self.backend.child_handle();
        let frontend_child = self.frontend.as_ref().map(|f| Arc::clone(f.child_handle()));
        let db_child = self.db.get().map(|db| Arc::clone(db.child_handle()));

        // Phase 1: Send SIGTERM to all processes (polite request to stop)
        debug!("Phase 1: Sending SIGTERM to all processes.");
        Self::send_sigterm("backend", backend_child).await;
        if let Some(ref handle) = frontend_child {
            Self::send_sigterm("frontend", handle).await;
        }
        if let Some(ref handle) = db_child {
            Self::send_sigterm("db", handle).await;
        }

        // Phase 2: Wait briefly for graceful exit (500ms)
        debug!("Phase 2: Waiting for graceful exit.");
        let wait_backend = Self::wait_for_child("backend", backend_child);
        let wait_frontend = async {
            if let Some(ref handle) = frontend_child {
                Self::wait_for_child("frontend", handle).await;
            }
        };
        let wait_db = async {
            if let Some(ref handle) = db_child {
                Self::wait_for_child("db", handle).await;
            }
        };
        let _ = timeout(Duration::from_millis(500), async {
            tokio::join!(wait_backend, wait_frontend, wait_db)
        })
        .await;

        // Phase 3: Force kill any remaining processes
        debug!("Phase 3: Force killing remaining processes.");
        Self::force_kill("backend", backend_child).await;
        if let Some(ref handle) = frontend_child {
            Self::force_kill("frontend", handle).await;
        }
        if let Some(ref handle) = db_child {
            Self::force_kill("db", handle).await;
        }

        debug!("All processes stopped.");
    }

    /// Get the status of all managed processes.
    /// Runs all three checks in parallel using tokio::join! to avoid blocking.
    pub async fn status(&self) -> (String, String, String) {
        let (frontend_status, backend_status, db_status) = tokio::join!(
            async {
                match self.frontend.as_ref() {
                    Some(f) => DevProcess::status(f.as_ref()).await.to_string(),
                    None => "n/a".to_string(),
                }
            },
            async { self.backend.status().await.to_string() },
            async {
                match self.db.get() {
                    Some(db) => DevProcess::status(db).await.to_string(),
                    None => "stopped".to_string(),
                }
            },
        );
        (frontend_status, backend_status, db_status)
    }

    /// Returns true if this project has a frontend (UI).
    pub fn has_ui(&self) -> bool {
        self.frontend.is_some()
    }

    /// Restart the backend (uvicorn) process with updated environment variables.
    pub async fn restart_uvicorn_with_env(
        &self,
        new_vars: HashMap<String, String>,
    ) -> Result<(), String> {
        self.backend.restart_with_env(new_vars).await
    }

    // -- Process lifecycle helpers (used for all child processes) --

    /// Send SIGTERM to a child process tree (polite shutdown request).
    /// On Windows, SIGTERM is not supported — falls back to SIGKILL (TerminateProcess).
    async fn send_sigterm(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let guard = child.lock().await;
        if let Some(child) = guard.as_ref()
            && let Some(pid) = child.id()
        {
            #[cfg(unix)]
            let signal = Signal::Term;
            #[cfg(not(unix))]
            let signal = Signal::Kill;
            debug!(process = name, pid, signal = ?signal, "Sending signal to process tree.");
            Self::send_signal_to_tree(pid, signal, name.to_string()).await;
        }
    }

    /// Wait for a child process to exit.
    async fn wait_for_child(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    debug!(process = name, ?status, "Child process already exited.");
                }
                Ok(None) => {
                    // Process still running, wait for it
                    match child.wait().await {
                        Ok(status) => debug!(process = name, ?status, "Child process exited."),
                        Err(err) => {
                            warn!(error = %err, process = name, "Failed to wait for child.");
                        }
                    }
                }
                Err(err) => warn!(error = %err, process = name, "Failed to check child status."),
            }
        }
    }

    /// Force kill a child process tree (SIGKILL).
    /// After the sysinfo-based tree kill, also uses tokio's cross-platform kill
    /// as a fallback (important on Windows where sysinfo's kill_with may fail).
    async fn force_kill(name: &str, child: &Arc<Mutex<Option<Child>>>) {
        let mut guard = child.lock().await;
        if let Some(mut child) = guard.take() {
            // Check if process is still running
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Already exited, nothing to do
                    debug!(
                        process = name,
                        "Process already exited, skipping force kill."
                    );
                }
                Ok(None) => {
                    // Still running, force kill via sysinfo tree walk
                    if let Some(pid) = child.id() {
                        debug!(process = name, pid, "Force killing process tree.");
                        Self::send_signal_to_tree(pid, Signal::Kill, name.to_string()).await;
                    }
                    // Fallback: use tokio's cross-platform kill for the direct child.
                    // This ensures termination even if sysinfo's kill_with fails (e.g. on Windows).
                    if let Err(err) = child.kill().await {
                        debug!(error = %err, process = name, "Tokio child.kill() fallback failed (process may have already exited).");
                    }
                    // Brief wait to allow kill to take effect
                    let _ = timeout(Duration::from_millis(100), child.wait()).await;
                }
                Err(err) => {
                    warn!(error = %err, process = name, "Failed to check process status.");
                }
            }
        }
    }

    // -- Signal/tree utilities (used for phased shutdown of all processes) --

    /// Send a signal to an entire process tree. This is a blocking operation.
    fn send_signal_to_tree_blocking(pid: u32, signal: Signal, label: &str) {
        let root_pid = Pid::from_u32(pid);
        let mut sys = System::new_all();
        sys.refresh_all();

        let Some(root_process) = sys.process(root_pid) else {
            debug!(
                process = label,
                pid, "Process not found, may have already exited."
            );
            return;
        };

        let root_start_time = root_process.start_time();
        let parents = build_parent_map(&sys);

        // Log the process tree we're about to signal
        debug!(
            process = label,
            root_pid = ?root_pid,
            root_name = ?root_process.name(),
            "Sending {:?} to process tree", signal
        );
        Self::log_process_tree(&sys, &parents, root_pid, root_start_time, label, 0);

        Self::send_signal_tree_recursive(&sys, &parents, root_pid, root_start_time, signal, label);
    }

    /// Log the process tree for debugging.
    fn log_process_tree(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        label: &str,
        depth: usize,
    ) {
        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time >= root_start_time {
                let indent = "  ".repeat(depth);
                debug!(
                    process = label,
                    "{}{:?} ({:?}) - started at {}",
                    indent,
                    pid,
                    process.name(),
                    process_start_time
                );
            }
        }

        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::log_process_tree(sys, parents, *child_pid, root_start_time, label, depth + 1);
            }
        }
    }

    /// Async wrapper for send_signal_to_tree that runs on a blocking thread.
    async fn send_signal_to_tree(pid: u32, signal: Signal, label: String) {
        let _ = tokio::task::spawn_blocking(move || {
            Self::send_signal_to_tree_blocking(pid, signal, &label);
        })
        .await;
    }

    /// Recursively send signal to process tree.
    fn send_signal_tree_recursive(
        sys: &System,
        parents: &HashMap<Pid, Vec<Pid>>,
        pid: Pid,
        root_start_time: u64,
        signal: Signal,
        label: &str,
    ) {
        // First, signal all children
        if let Some(children) = parents.get(&pid) {
            for child_pid in children {
                Self::send_signal_tree_recursive(
                    sys,
                    parents,
                    *child_pid,
                    root_start_time,
                    signal,
                    label,
                );
            }
        }

        // Then signal this process
        if let Some(process) = sys.process(pid) {
            let process_start_time = process.start_time();
            if process_start_time < root_start_time {
                return;
            }
            let name = process.name();
            if process.kill_with(signal).unwrap_or(false) {
                debug!(pid = ?pid, process_name = ?name, ?signal, process = label, "Sent signal to process.");
            }
        }
    }
}
