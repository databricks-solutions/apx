//! Multi-worker supervisor: spawn, monitor, and restart worker processes.
//!
//! The supervisor NEVER imports or calls PyO3. Python is initialized only
//! in worker processes. See the architectural boundary note in the plan.

use super::ipc::channel::{self, WorkerChannel};
use super::ipc::protocol::AppModule;
use super::ipc::protocol::{IpcMessage, Nonce, WorkerBootstrap};
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
    /// Maximum concurrent requests per worker (`None` → framework default).
    pub max_concurrent: Option<usize>,
    /// Event loop policy: `"asyncio"` or `"uvloop"`.
    pub loop_policy: String,
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
        source: super::ipc::protocol::IpcError,
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

/// How long to wait for workers to drain in-flight requests.
///
/// Databricks Apps enforces a 15-second SIGTERM budget. Keep the total
/// shutdown budget (drain + SIGKILL_TIMEOUT) well under that limit.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);

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
        name: "apx.supervisor.started",
        workers = config.workers,
        host = %config.host,
        port = config.port,
        app = %config.app_module,
        "starting supervisor"
    );

    let mut workers = Vec::with_capacity(config.workers);
    for i in 0..config.workers {
        let relay = i == 0;
        let worker = spawn_worker(i, &config, &nonce, socket_dir.path(), relay).await?;
        workers.push(worker);
    }

    // Wait for telemetry config relay from worker 0.
    let (system_config, process_config) = match tokio::time::timeout(
        WORKER_READINESS_TIMEOUT,
        workers[0].channel.recv(),
    )
    .await
    {
        Ok(Ok(IpcMessage::TelemetryConfig(relay))) => {
            tracing::info!(name: "apx.supervisor.telemetry_config_received", "received telemetry config relay from worker 0");
            (relay.system, relay.process)
        }
        Ok(Ok(other)) => {
            tracing::warn!(
                name: "apx.supervisor.telemetry_config_unexpected",
                ?other,
                "expected TelemetryConfig from worker 0, falling back to defaults"
            );
            (
                crate::telemetry::config::default_system_config(),
                crate::telemetry::config::default_process_config(),
            )
        }
        Ok(Err(e)) => {
            tracing::warn!(name: "apx.supervisor.telemetry_config_ipc_error", %e, "IPC error reading telemetry config, falling back to defaults");
            (
                crate::telemetry::config::default_system_config(),
                crate::telemetry::config::default_process_config(),
            )
        }
        Err(_) => {
            tracing::warn!(
                name: "apx.supervisor.telemetry_config_timeout",
                "timeout waiting for telemetry config relay, falling back to defaults"
            );
            (
                crate::telemetry::config::default_system_config(),
                crate::telemetry::config::default_process_config(),
            )
        }
    };

    let _system_metrics_handle =
        crate::telemetry::system_metrics::spawn_system_metrics(&system_config);
    let _supervisor_process_handle =
        crate::telemetry::process_metrics::spawn_process_metrics(&process_config);

    // Run monitor and shutdown signal in parallel.
    // Monitor returns on AllWorkersCrashed; shutdown signal returns on SIGTERM/SIGINT.
    tokio::select! {
        result = monitor_workers(&mut workers, &config, &nonce, socket_dir.path()) => {
            result?;
        }
        () = shutdown_signal() => {
            tracing::info!(name: "apx.supervisor.shutdown", "shutdown signal received, stopping workers");
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
pub(crate) struct WorkerHandle {
    /// Worker index (0-based).
    index: usize,
    /// Child process handle.
    pub(crate) child: tokio::process::Child,
    /// IPC channel to the worker.
    channel: WorkerChannel,
    /// Number of restarts for this worker slot.
    restart_count: usize,
    /// Last restart time.
    last_restart: std::time::Instant,
}

impl WorkerHandle {
    /// Test-only constructor. Production code builds handles in `spawn_worker`.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        index: usize,
        child: tokio::process::Child,
        channel: WorkerChannel,
    ) -> Self {
        Self {
            index,
            child,
            channel,
            restart_count: 0,
            last_restart: std::time::Instant::now(),
        }
    }
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
    relay_telemetry: bool,
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

    // Prefer finding "apx" on PATH so this works when the binary is a
    // pip-installed Python entry-point script (where current_exe() returns
    // the Python interpreter, not "apx").  Fall back to current_exe() for
    // the cargo-built native binary case.
    let exe = which::which("apx")
        .unwrap_or_else(|_| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("apx")));

    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .arg("--host")
        .arg(&config.host)
        .arg("--port")
        .arg(config.port.to_string())
        .arg("--timeout")
        .arg(config.request_timeout.as_secs().to_string());

    cmd.arg(config.app_module.as_str())
        .arg("--loop")
        .arg(&config.loop_policy);

    cmd.current_dir(&config.app_dir)
        .env("APX_WORKER_NONCE", nonce.as_str())
        .env("APX_WORKER_SOCK", sock_str)
        .env("APX_WORKER_ID", index.to_string())
        .env("PYTHONPATH", &config.app_dir);

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

    tracing::info!(name: "apx.supervisor.worker_spawned", worker = index, pid = child.id(), "spawned worker");

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
        max_concurrent: config.max_concurrent,
        nonce: nonce.clone(),
        loop_policy: config.loop_policy.clone(),
        relay_telemetry,
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
            tracing::info!(name: "apx.supervisor.worker_ready", worker = index, "worker ready");
        }
        other => {
            return Err(SupervisorError::Ipc {
                index,
                source: super::ipc::protocol::IpcError::Io(std::io::Error::other(format!(
                    "expected Ready, got {other:?}"
                ))),
            });
        }
    }

    Ok(WorkerHandle {
        index,
        child,
        channel,
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

        tracing::error!(name: "apx.supervisor.worker_error", worker = exited_index, ?status, "worker exited");

        let handle = &mut workers[exited_index];

        // Reset restart counter if the worker lived long enough.
        if handle.last_restart.elapsed() > RESTART_WINDOW {
            handle.restart_count = 0;
        }

        handle.restart_count += 1;

        if handle.restart_count > MAX_RESTARTS_PER_WORKER {
            tracing::error!(
                name: "apx.supervisor.max_restarts",
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
            name: "apx.supervisor.worker_restart",
            worker = exited_index,
            attempt = handle.restart_count,
            "restarting worker"
        );

        match spawn_worker(exited_index, config, nonce, socket_dir, false).await {
            Ok(new_handle) => {
                let restart_count = handle.restart_count;
                workers[exited_index] = new_handle;
                workers[exited_index].restart_count = restart_count;
                workers[exited_index].last_restart = std::time::Instant::now();
            }
            Err(e) => {
                tracing::error!(name: "apx.supervisor.worker_restart_failed", worker = exited_index, error = %e, "failed to restart worker");
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

/// How long to wait after sending SIGTERM before sending SIGKILL.
const SIGKILL_TIMEOUT: Duration = Duration::from_secs(3);

/// Gracefully shut down all workers.
///
/// Phase 1: Send `IpcMessage::Drain` to all workers.
/// Phase 2: Wait for `IpcMessage::Drained` from all (up to `GRACEFUL_SHUTDOWN_TIMEOUT`).
/// Phase 3: If timeout, send SIGTERM to remaining workers.
/// Phase 4: Wait `SIGKILL_TIMEOUT`, then SIGKILL.
pub(crate) async fn shutdown_workers(workers: &mut [WorkerHandle]) {
    // Phase 1: Send Drain over IPC.
    for worker in workers.iter_mut() {
        if let Err(e) = worker.channel.send(&IpcMessage::Drain).await {
            tracing::debug!(name: "apx.supervisor.drain_send_failed", worker = worker.index, error = %e, "failed to send Drain");
        }
    }

    // Phase 2: Wait for Drained from all workers (or timeout).
    let drain_all = async {
        for worker in workers.iter_mut() {
            match worker.channel.recv().await {
                Ok(IpcMessage::Drained) => {
                    tracing::info!(name: "apx.supervisor.drained", worker = worker.index, "worker drained");
                }
                Ok(msg) => {
                    tracing::debug!(
                        name: "apx.supervisor.drain_unexpected_message",
                        worker = worker.index,
                        ?msg,
                        "unexpected message during drain"
                    );
                }
                Err(e) => {
                    tracing::debug!(name: "apx.supervisor.drain_ipc_error", worker = worker.index, error = %e, "IPC error during drain");
                }
            }
        }
    };
    if tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, drain_all)
        .await
        .is_ok()
    {
        // All workers drained — wait for process exit.
        let wait_all = async {
            for worker in workers.iter_mut() {
                let _ = worker.child.wait().await;
            }
        };
        if tokio::time::timeout(SIGKILL_TIMEOUT, wait_all)
            .await
            .is_err()
        {
            tracing::warn!(name: "apx.supervisor.sigkill", "workers did not exit after drain, sending SIGKILL");
            for worker in workers.iter_mut() {
                let _ = worker.child.kill().await;
            }
        }
        return;
    }

    // Phase 3: SIGTERM remaining workers that didn't drain in time.
    tracing::warn!(name: "apx.supervisor.drain_timeout", "drain timeout, sending SIGTERM to remaining workers");
    for worker in workers.iter() {
        if let Some(pid) = worker.child.id() {
            send_signal(pid, Signal::Term).await;
        }
    }

    // Phase 4: Wait briefly, then SIGKILL.
    let wait_all = async {
        for worker in workers.iter_mut() {
            let _ = worker.child.wait().await;
        }
    };
    if tokio::time::timeout(SIGKILL_TIMEOUT, wait_all)
        .await
        .is_ok()
    {
        return;
    }

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
use super::signal::shutdown_signal;

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
            max_concurrent: None,
            loop_policy: "uvloop".to_owned(),
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
            max_concurrent: None,
            loop_policy: "uvloop".to_owned(),
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
            max_concurrent: None,
            loop_policy: "uvloop".to_owned(),
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
