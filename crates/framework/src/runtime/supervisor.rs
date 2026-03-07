//! Multi-worker supervisor: spawn, monitor, and restart worker processes.
//!
//! The supervisor NEVER imports or calls PyO3. Python is initialized only
//! in worker processes. See the architectural boundary note in the plan.

use crate::bridge::CorsConfig;
use crate::ipc::channel::{self, WorkerChannel};
use crate::ipc::protocol::{IpcMessage, Nonce, WorkerBootstrap};
use crate::route::AppModule;
use std::path::PathBuf;
use std::time::Duration;
use sysinfo::{Pid, Signal, System};
use tokio::process::Command;

/// Supervisor configuration.
#[derive(Debug)]
pub struct SupervisorConfig {
    /// Host to bind workers to.
    pub host: String,
    /// Port for workers to bind (all share via `SO_REUSEPORT`).
    pub port: u16,
    /// Number of worker processes.
    pub workers: usize,
    /// Python module path (validated).
    pub app_module: AppModule,
    /// Working directory for workers.
    pub app_dir: PathBuf,
    /// Per-request timeout passed to workers.
    pub request_timeout: Duration,
    /// Path to pre-built manifest JSON (skips live FastAPI discovery).
    pub manifest_path: Option<PathBuf>,
    /// CORS policy for workers.
    pub cors: CorsConfig,
}

/// What went wrong with supervisor config validation.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum ConfigError {
    /// Worker count was zero.
    #[error("workers must be > 0, got {0}")]
    ZeroWorkers(usize),
    /// Port was zero.
    #[error("port must be > 0")]
    ZeroPort,
}

/// Supervisor-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// Failed to spawn a worker process.
    #[error("failed to spawn worker {index}: {source}")]
    WorkerSpawn {
        /// Worker index.
        index: usize,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Failed to create IPC socket for a worker.
    #[error("failed to create IPC socket for worker {index}: {source}")]
    IpcCreate {
        /// Worker index.
        index: usize,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// IPC communication error with a worker.
    #[error("worker {index} IPC error: {source}")]
    Ipc {
        /// Worker index.
        index: usize,
        /// Underlying IPC error.
        source: crate::ipc::protocol::IpcError,
    },
    /// Worker did not send Ready within timeout.
    #[error("worker {index} did not send Ready within timeout")]
    ReadinessTimeout {
        /// Worker index.
        index: usize,
    },
    /// All workers crashed within the restart window.
    #[error("all {count} workers crashed within restart window")]
    AllWorkersCrashed {
        /// Number of workers.
        count: usize,
    },
    /// Invalid config.
    #[error("invalid config: {0}")]
    Config(#[from] ConfigError),
}

/// Restart policy constants.
const MAX_RESTARTS_PER_WORKER: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const WORKER_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait after SIGTERM before sending SIGKILL.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the multi-worker supervisor.
///
/// Spawns N worker processes, monitors them, and restarts on crash.
/// Returns when all workers have been shut down (graceful or error).
///
/// # Errors
///
/// Returns an error on config validation failure, worker spawn failure,
/// or if all workers crash.
pub async fn run_supervisor(config: SupervisorConfig) -> Result<(), SupervisorError> {
    validate_config(&config)?;

    let nonce = Nonce::generate();
    let socket_dir = tempfile::tempdir().map_err(|e| SupervisorError::IpcCreate {
        index: 0,
        source: e,
    })?;

    tracing::info!(
        workers = config.workers,
        host = %config.host,
        port = config.port,
        app = %config.app_module,
        "starting supervisor"
    );

    let mut workers = Vec::with_capacity(config.workers);
    for i in 0..config.workers {
        let worker = spawn_worker(i, &config, &nonce, socket_dir.path()).await?;
        workers.push(worker);
    }

    // Run monitor and shutdown signal in parallel.
    // Monitor returns on AllWorkersCrashed; shutdown signal returns on SIGTERM/SIGINT.
    tokio::select! {
        result = monitor_workers(&mut workers, &config, &nonce, socket_dir.path()) => {
            result?;
        }
        () = shutdown_signal() => {
            tracing::info!("shutdown signal received, stopping workers");
            shutdown_workers(&mut workers).await;
        }
    }

    Ok(())
}

/// Validate supervisor config.
fn validate_config(config: &SupervisorConfig) -> Result<(), SupervisorError> {
    if config.workers == 0 {
        return Err(ConfigError::ZeroWorkers(config.workers).into());
    }
    if config.port == 0 {
        return Err(ConfigError::ZeroPort.into());
    }
    Ok(())
}

/// State for a single worker process.
struct WorkerHandle {
    /// Worker index (0-based).
    index: usize,
    /// Child process handle.
    child: tokio::process::Child,
    /// IPC channel to the worker.
    _channel: WorkerChannel,
    /// Number of restarts for this worker slot.
    restart_count: usize,
    /// Last restart time.
    last_restart: std::time::Instant,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("index", &self.index)
            .field("restart_count", &self.restart_count)
            .finish_non_exhaustive()
    }
}

/// Spawn a single worker process and complete the bootstrap handshake.
async fn spawn_worker(
    index: usize,
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<WorkerHandle, SupervisorError> {
    let sock_path = socket_dir.join(format!("worker-{index}.sock"));
    let sock_str = sock_path
        .to_str()
        .ok_or_else(|| SupervisorError::IpcCreate {
            index,
            source: std::io::Error::other("socket path is not UTF-8"),
        })?;

    // Remove stale socket if it exists (from a previous worker in this slot).
    let _ = std::fs::remove_file(&sock_path);

    let listener = channel::listen(sock_str).map_err(|e| SupervisorError::IpcCreate {
        index,
        source: std::io::Error::other(e.to_string()),
    })?;

    let current_exe =
        std::env::current_exe().map_err(|e| SupervisorError::WorkerSpawn { index, source: e })?;

    let mut cmd = Command::new(current_exe);
    cmd.arg("serve")
        .arg("--app")
        .arg(config.app_module.as_str())
        .arg("--host")
        .arg(&config.host)
        .arg("--port")
        .arg(config.port.to_string())
        .arg("--timeout")
        .arg(config.request_timeout.as_secs().to_string())
        .current_dir(&config.app_dir)
        .env("APX_WORKER_NONCE", nonce.as_str())
        .env("APX_WORKER_SOCK", sock_str);

    if let Some(manifest) = &config.manifest_path {
        cmd.arg("--manifest").arg(manifest);
    }

    // Propagate OTEL env vars.
    for (key, value) in std::env::vars() {
        if key.starts_with("OTEL_") {
            cmd.env(&key, &value);
        }
    }

    let child = cmd
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| SupervisorError::WorkerSpawn { index, source: e })?;

    tracing::info!(worker = index, pid = child.id(), "spawned worker");

    // Accept connection and complete bootstrap handshake.
    let mut channel = tokio::time::timeout(WORKER_READINESS_TIMEOUT, channel::accept(&listener))
        .await
        .map_err(|_| SupervisorError::ReadinessTimeout { index })?
        .map_err(|e| SupervisorError::Ipc { index, source: e })?;

    let bootstrap = WorkerBootstrap {
        host: config.host.clone(),
        port: config.port,
        app_module: config.app_module.clone(),
        request_timeout_secs: config.request_timeout.as_secs(),
        nonce: nonce.clone(),
        manifest_path: config.manifest_path.clone(),
        cors: config.cors,
    };

    channel
        .send(&IpcMessage::Bootstrap(bootstrap))
        .await
        .map_err(|e| SupervisorError::Ipc { index, source: e })?;

    // Wait for Ready signal.
    let msg = tokio::time::timeout(WORKER_READINESS_TIMEOUT, channel.recv())
        .await
        .map_err(|_| SupervisorError::ReadinessTimeout { index })?
        .map_err(|e| SupervisorError::Ipc { index, source: e })?;

    match msg {
        IpcMessage::Ready => {
            tracing::info!(worker = index, "worker ready");
        }
        IpcMessage::Bootstrap(_) => {
            return Err(SupervisorError::Ipc {
                index,
                source: crate::ipc::protocol::IpcError::Io(std::io::Error::other(
                    "worker sent Bootstrap instead of Ready",
                )),
            });
        }
    }

    Ok(WorkerHandle {
        index,
        child,
        _channel: channel,
        restart_count: 0,
        last_restart: std::time::Instant::now(),
    })
}

/// Monitor workers and restart crashed ones.
async fn monitor_workers(
    workers: &mut [WorkerHandle],
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<(), SupervisorError> {
    loop {
        let (exited_index, status) = wait_for_any_exit(workers).await;

        tracing::error!(worker = exited_index, ?status, "worker exited");

        let handle = &mut workers[exited_index];

        // Reset restart counter if the worker lived long enough.
        if handle.last_restart.elapsed() > RESTART_WINDOW {
            handle.restart_count = 0;
        }

        handle.restart_count += 1;

        if handle.restart_count > MAX_RESTARTS_PER_WORKER {
            tracing::error!(
                worker = exited_index,
                restarts = handle.restart_count,
                "worker exceeded max restarts"
            );

            let all_dead = workers
                .iter_mut()
                .all(|w| w.child.try_wait().map(|s| s.is_some()).unwrap_or(true));

            if all_dead {
                return Err(SupervisorError::AllWorkersCrashed {
                    count: config.workers,
                });
            }
            continue;
        }

        tracing::info!(
            worker = exited_index,
            attempt = handle.restart_count,
            "restarting worker"
        );

        match spawn_worker(exited_index, config, nonce, socket_dir).await {
            Ok(new_handle) => {
                let restart_count = handle.restart_count;
                workers[exited_index] = new_handle;
                workers[exited_index].restart_count = restart_count;
                workers[exited_index].last_restart = std::time::Instant::now();
            }
            Err(e) => {
                tracing::error!(worker = exited_index, error = %e, "failed to restart worker");
            }
        }
    }
}

/// Wait for any worker process to exit, return its index and exit status.
async fn wait_for_any_exit(
    workers: &mut [WorkerHandle],
) -> (usize, Option<std::process::ExitStatus>) {
    let futs: Vec<_> = workers
        .iter_mut()
        .enumerate()
        .map(|(i, w)| Box::pin(async move { (i, w.child.wait().await) }))
        .collect();

    let ((index, result), _, _) = futures_util::future::select_all(futs).await;
    match result {
        Ok(status) => (index, Some(status)),
        Err(_) => (index, None),
    }
}

/// Gracefully shut down all workers.
///
/// Phase 1: Send SIGTERM via `sysinfo` (same pattern as `crates/core/src/dev/process.rs`).
/// Phase 2: Wait up to [`GRACEFUL_SHUTDOWN_TIMEOUT`].
/// Phase 3: Force kill remaining via `child.kill()`.
async fn shutdown_workers(workers: &mut [WorkerHandle]) {
    // Phase 1: SIGTERM via sysinfo.
    for worker in workers.iter() {
        if let Some(pid) = worker.child.id() {
            send_signal(pid, Signal::Term).await;
        }
    }

    // Phase 2: Wait for graceful exit (or timeout).
    let wait_all = async {
        for worker in workers.iter_mut() {
            let _ = worker.child.wait().await;
        }
    };
    if tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, wait_all)
        .await
        .is_ok()
    {
        return;
    }

    // Phase 3: SIGKILL remaining.
    for worker in workers.iter_mut() {
        let _ = worker.child.kill().await;
    }
}

/// Send a signal to a process using `sysinfo` (no unsafe code).
///
/// Same pattern as `ProcessManager::send_signal_to_tree` in `crates/core`.
async fn send_signal(pid: u32, signal: Signal) {
    let _ = tokio::task::spawn_blocking(move || {
        let mut sys = System::new();
        sys.refresh_processes(
            sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
        );
        if let Some(process) = sys.process(Pid::from_u32(pid)) {
            let _ = process.kill_with(signal);
        }
    })
    .await;
}

/// Re-export shared shutdown signal for supervisor use.
use crate::signal::shutdown_signal;

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    /// Verify supervisor.rs does not import pyo3 (architectural boundary).
    ///
    /// Checks only non-test code by splitting on `#[cfg(test)]`.
    #[test]
    fn supervisor_has_no_pyo3_imports() {
        let full_source = include_str!("supervisor.rs");
        // Only check the production code (before the test module).
        let source = full_source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(full_source);

        assert!(!source.contains("use pyo3"), "must not import pyo3");
        assert!(!source.contains("Python::"), "must not use Python::");
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn validate_config_valid() {
        let config = SupervisorConfig {
            host: "127.0.0.1".to_owned(),
            port: 8000,
            workers: 4,
            app_module: AppModule::new("backend.app").unwrap(),
            app_dir: PathBuf::from("/app"),
            request_timeout: Duration::from_secs(30),
            manifest_path: None,
            cors: CorsConfig::default(),
        };
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_zero_workers() {
        let config = SupervisorConfig {
            host: "127.0.0.1".to_owned(),
            port: 8000,
            workers: 0,
            app_module: AppModule::new("backend.app").unwrap(),
            app_dir: PathBuf::from("/app"),
            request_timeout: Duration::from_secs(30),
            manifest_path: None,
            cors: CorsConfig::default(),
        };
        let err = validate_config(&config).unwrap_err();
        assert!(matches!(
            err,
            SupervisorError::Config(ConfigError::ZeroWorkers(0))
        ));
    }

    #[test]
    fn validate_config_zero_port() {
        let config = SupervisorConfig {
            host: "127.0.0.1".to_owned(),
            port: 0,
            workers: 4,
            app_module: AppModule::new("backend.app").unwrap(),
            app_dir: PathBuf::from("/app"),
            request_timeout: Duration::from_secs(30),
            manifest_path: None,
            cors: CorsConfig::default(),
        };
        let err = validate_config(&config).unwrap_err();
        assert!(matches!(
            err,
            SupervisorError::Config(ConfigError::ZeroPort)
        ));
    }

    #[test]
    fn config_error_display_zero_workers() {
        let err = ConfigError::ZeroWorkers(0);
        let msg = format!("{err}");
        assert!(msg.contains("workers"));
        assert!(msg.contains('0'));
    }

    #[test]
    fn config_error_display_zero_port() {
        let err = ConfigError::ZeroPort;
        let msg = format!("{err}");
        assert!(msg.contains("port"));
    }

    #[test]
    fn supervisor_error_display_config() {
        let err = SupervisorError::Config(ConfigError::ZeroPort);
        let msg = format!("{err}");
        assert!(msg.contains("port"));
    }
}
