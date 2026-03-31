//! Multi-worker supervisor: spawn, monitor, and restart worker processes.
//!
//! The supervisor NEVER imports or calls PyO3. Python is initialized only
//! in worker processes. See the architectural boundary note in the plan.

use super::dev_watcher::{DevWatcher, DevWatcherError};
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
    /// Enable dev-mode file watcher for hot reload.
    pub dev_mode: bool,
    /// Maximum time to wait for workers to drain in-flight requests before
    /// warning and killing them.
    pub drain_timeout: Duration,
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
    /// Worker reported a startup failure over IPC (e.g. Python import error).
    #[error("worker {index} failed to start: {error}")]
    WorkerStartupFailed {
        /// Worker index.
        index: usize,
        /// Error message from the worker.
        error: String,
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
    /// Dev file watcher failed to start.
    #[error("dev watcher: {0}")]
    DevWatcher(#[from] DevWatcherError),
}

/// Restart policy constants.
const MAX_RESTARTS_PER_WORKER: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const WORKER_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Operational state of the dev-mode supervisor.
///
/// In production mode this is always `Serving`. In dev mode the supervisor
/// transitions to `Degraded` when workers fail to start, and back to
/// `Serving` when a file change triggers a successful reload.
enum SupervisorMode {
    /// All workers are running and serving requests.
    Serving,
    /// Workers failed to start; waiting for a file change to retry.
    Degraded,
}

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

    let startup_start = std::time::Instant::now();
    let nonce = Nonce::generate();
    let socket_dir = tempfile::tempdir().map_err(|e| SupervisorError::IpcCreate {
        index: 0,
        source: e,
    })?;

    let has_otel = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();
    let telemetry_label = if has_otel { "on" } else { "off" };

    tracing::info!(
        name: "apx.supervisor.started",
        workers = config.workers,
        host = %config.host,
        port = config.port,
        app = %config.app_module,
        "starting server with {} event loop, telemetry {}, {} worker(s)",
        config.loop_policy,
        telemetry_label,
        config.workers,
    );

    let mut workers = Vec::with_capacity(config.workers);
    let mut mode = match try_spawn_all(&mut workers, &config, &nonce, socket_dir.path()).await {
        Ok(()) => SupervisorMode::Serving,
        Err(e) if config.dev_mode => {
            tracing::error!(
                name: "apx.supervisor.startup_failed",
                "startup failed, waiting for file change to retry\n{e}",
            );
            SupervisorMode::Degraded
        }
        Err(e) => return Err(e),
    };

    if matches!(mode, SupervisorMode::Serving) {
        let (system_config, process_config) = recv_telemetry_config(&mut workers[0].channel).await;
        log_ready(startup_start);
        let _system_metrics_handle =
            crate::telemetry::system_metrics::spawn_system_metrics(&system_config);
        let _supervisor_process_handle =
            crate::telemetry::process_metrics::spawn_process_metrics(&process_config);
    }

    let mut dev_watcher = if config.dev_mode {
        Some(DevWatcher::new(&config.app_dir)?)
    } else {
        None
    };

    loop {
        tokio::select! {
            (idx, status) = wait_for_any_exit(&mut workers),
                if matches!(mode, SupervisorMode::Serving) && !workers.is_empty() =>
            {
                if config.dev_mode {
                    tracing::error!(
                        name: "apx.supervisor.worker_error",
                        worker = idx,
                        ?status,
                        "worker exited, waiting for file change to retry",
                    );
                    mode = SupervisorMode::Degraded;
                } else {
                    handle_worker_exit(idx, status, &mut workers, &config, &nonce, socket_dir.path()).await?;
                }
            }
            () = shutdown_signal() => {
                tracing::info!(name: "apx.supervisor.shutdown", "shutdown signal received, stopping workers");
                shutdown_workers(&mut workers, config.drain_timeout).await;
                break;
            }
            Some(info) = recv_dev_reload(&mut dev_watcher) => {
                mode = try_reload(&mut workers, &config, &nonce, socket_dir.path(), &info).await;
            }
        }
    }

    Ok(())
}

/// Spawn all workers for initial startup.
async fn try_spawn_all(
    workers: &mut Vec<WorkerHandle>,
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<(), SupervisorError> {
    for i in 0..config.workers {
        let relay = i == 0;
        let worker = spawn_worker(i, config, nonce, socket_dir, relay).await?;
        workers.push(worker);
    }
    Ok(())
}

/// Log the "server ready" message with elapsed time.
fn log_ready(startup_start: std::time::Instant) {
    let startup_ms = startup_start.elapsed().as_millis();
    tracing::info!(
        name: "apx.supervisor.ready",
        "server ready in {}ms",
        startup_ms,
    );
}

/// Kill existing workers and attempt to respawn them.
///
/// Returns `Serving` on success or `Degraded` on failure.
async fn try_reload(
    workers: &mut Vec<WorkerHandle>,
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
    info: &super::dev_watcher::ReloadInfo,
) -> SupervisorMode {
    let summary = format_reload_files(&info.files);
    tracing::info!(
        name: "apx.supervisor.dev_reload",
        "reload triggered by changes in {}",
        summary,
    );
    tracing::info!(name: "apx.supervisor.dev_reload_stop", "stopping workers for reload");
    let reload_start = std::time::Instant::now();
    kill_workers(workers, config.drain_timeout).await;

    match respawn_all_workers(workers, config, nonce, socket_dir).await {
        Ok(()) => {
            log_ready(reload_start);
            SupervisorMode::Serving
        }
        Err(e) => {
            tracing::error!(
                name: "apx.supervisor.reload_failed",
                "reload failed, waiting for file change to retry\n{e}",
            );
            SupervisorMode::Degraded
        }
    }
}

/// Receive telemetry config from worker 0, falling back to defaults on any
/// IPC failure or timeout.
async fn recv_telemetry_config(
    channel: &mut WorkerChannel,
) -> (
    crate::telemetry::config::SystemConfig,
    crate::telemetry::config::ProcessConfig,
) {
    let defaults = || {
        (
            crate::telemetry::config::default_system_config(),
            crate::telemetry::config::default_process_config(),
        )
    };

    match tokio::time::timeout(WORKER_READINESS_TIMEOUT, channel.recv()).await {
        Ok(Ok(IpcMessage::TelemetryConfig(relay))) => {
            tracing::debug!(name: "apx.supervisor.telemetry_config_received", "received telemetry config from worker 0");
            (relay.system, relay.process)
        }
        Ok(Ok(other)) => {
            tracing::warn!(name: "apx.supervisor.telemetry_config_unexpected", ?other, "expected TelemetryConfig, falling back to defaults");
            defaults()
        }
        Ok(Err(e)) => {
            tracing::warn!(name: "apx.supervisor.telemetry_config_ipc_error", %e, "IPC error, falling back to defaults");
            defaults()
        }
        Err(_) => {
            tracing::warn!(name: "apx.supervisor.telemetry_config_timeout", "timeout waiting for telemetry config, falling back to defaults");
            defaults()
        }
    }
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

    let _ = std::fs::remove_file(&sock_path);

    let listener = channel::listen(sock_str).map_err(|e| SupervisorError::IpcCreate {
        index,
        source: std::io::Error::other(e.to_string()),
    })?;

    let child = spawn_worker_process(index, config, nonce, sock_str)?;
    tracing::debug!(name: "apx.supervisor.worker_spawned", worker = index, pid = child.id(), "spawned worker");

    let channel = bootstrap_worker(index, config, nonce, &listener, relay_telemetry).await?;

    Ok(WorkerHandle {
        index,
        child,
        channel,
        restart_count: 0,
        last_restart: std::time::Instant::now(),
    })
}

/// Build and spawn the worker child process.
fn spawn_worker_process(
    index: usize,
    config: &SupervisorConfig,
    nonce: &Nonce,
    sock_str: &str,
) -> Result<tokio::process::Child, SupervisorError> {
    let exe = which::which("apx")
        .unwrap_or_else(|_| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("apx")));

    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .arg("--host")
        .arg(&config.host)
        .arg("--port")
        .arg(config.port.to_string())
        .arg("--timeout")
        .arg(config.request_timeout.as_secs().to_string())
        .arg(config.app_module.as_str())
        .arg("--loop")
        .arg(&config.loop_policy)
        .current_dir(&config.app_dir)
        .env("APX_WORKER_NONCE", nonce.as_str())
        .env("APX_WORKER_SOCK", sock_str)
        .env("APX_WORKER_ID", index.to_string())
        .env("PYTHONPATH", &config.app_dir)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    for (key, value) in std::env::vars() {
        if key.starts_with("OTEL_") {
            cmd.env(&key, &value);
        }
    }

    cmd.spawn()
        .map_err(|e| SupervisorError::WorkerSpawn { index, source: e })
}

/// Accept the IPC connection and complete the bootstrap handshake.
async fn bootstrap_worker(
    index: usize,
    config: &SupervisorConfig,
    nonce: &Nonce,
    listener: &tokio::net::UnixListener,
    relay_telemetry: bool,
) -> Result<WorkerChannel, SupervisorError> {
    let mut channel = tokio::time::timeout(WORKER_READINESS_TIMEOUT, channel::accept(listener))
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
        drain_timeout_secs: if config.dev_mode {
            0
        } else {
            config.drain_timeout.as_secs()
        },
        dev_mode: config.dev_mode,
    };

    channel
        .send(&IpcMessage::Bootstrap(bootstrap))
        .await
        .map_err(|e| SupervisorError::Ipc { index, source: e })?;

    let msg = tokio::time::timeout(WORKER_READINESS_TIMEOUT, channel.recv())
        .await
        .map_err(|_| SupervisorError::ReadinessTimeout { index })?
        .map_err(|e| SupervisorError::Ipc { index, source: e })?;

    match msg {
        IpcMessage::Ready => {
            tracing::debug!(name: "apx.supervisor.worker_ready", worker = index, "worker ready");
            Ok(channel)
        }
        IpcMessage::StartupFailed { error } => {
            Err(SupervisorError::WorkerStartupFailed { index, error })
        }
        other => Err(SupervisorError::Ipc {
            index,
            source: super::ipc::protocol::IpcError::Io(std::io::Error::other(format!(
                "expected Ready or StartupFailed, got {other:?}"
            ))),
        }),
    }
}

/// Handle a single worker exit: apply restart policy and respawn if allowed.
async fn handle_worker_exit(
    exited_index: usize,
    status: Option<std::process::ExitStatus>,
    workers: &mut [WorkerHandle],
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<(), SupervisorError> {
    tracing::error!(name: "apx.supervisor.worker_error", worker = exited_index, ?status, "worker exited");

    let handle = &mut workers[exited_index];
    if handle.last_restart.elapsed() > RESTART_WINDOW {
        handle.restart_count = 0;
    }
    handle.restart_count += 1;

    if handle.restart_count <= MAX_RESTARTS_PER_WORKER {
        return respawn_one_worker(exited_index, workers, config, nonce, socket_dir).await;
    }

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
    Ok(())
}

/// Attempt to respawn a single crashed worker, preserving its restart count.
async fn respawn_one_worker(
    index: usize,
    workers: &mut [WorkerHandle],
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<(), SupervisorError> {
    let restart_count = workers[index].restart_count;
    tracing::info!(
        name: "apx.supervisor.worker_restart",
        worker = index,
        attempt = restart_count,
        "restarting worker"
    );

    match spawn_worker(index, config, nonce, socket_dir, false).await {
        Ok(new_handle) => {
            workers[index] = new_handle;
            workers[index].restart_count = restart_count;
            workers[index].last_restart = std::time::Instant::now();
        }
        Err(e) => {
            tracing::error!(name: "apx.supervisor.worker_restart_failed", worker = index, error = %e, "failed to restart worker");
        }
    }
    Ok(())
}

/// Receive a dev reload signal, or pend forever when no watcher is active.
async fn recv_dev_reload(
    watcher: &mut Option<DevWatcher>,
) -> Option<super::dev_watcher::ReloadInfo> {
    match watcher.as_mut() {
        Some(w) => w.recv().await,
        None => std::future::pending().await,
    }
}

/// Format a list of changed files for human-readable log output.
///
/// Returns `"path/to/file.py"` for a single file, or
/// `"path/to/file.py (+N more)"` when multiple files changed.
fn format_reload_files(files: &[PathBuf]) -> String {
    match files.len() {
        0 => "unknown files".to_owned(),
        1 => files[0].display().to_string(),
        n => format!("{} (+{} more)", files[0].display(), n - 1),
    }
}

/// Shut down all workers and respawn them fresh.
async fn respawn_all_workers(
    workers: &mut Vec<WorkerHandle>,
    config: &SupervisorConfig,
    nonce: &Nonce,
    socket_dir: &std::path::Path,
) -> Result<(), SupervisorError> {
    workers.clear();
    for i in 0..config.workers {
        let relay = i == 0;
        let worker = spawn_worker(i, config, nonce, socket_dir, relay).await?;
        workers.push(worker);
    }
    Ok(())
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
/// Phase 2: Wait for `IpcMessage::Drained` from all (up to `drain_timeout`).
/// Phase 3: If timeout, warn, send SIGTERM, wait `SIGKILL_TIMEOUT`, then SIGKILL.
pub(crate) async fn shutdown_workers(workers: &mut [WorkerHandle], drain_timeout: Duration) {
    send_drain_to_all(workers).await;

    if wait_for_all_drained(workers, drain_timeout).await {
        wait_or_kill(workers).await;
        return;
    }

    tracing::warn!(
        name: "apx.supervisor.drain_timeout",
        timeout_secs = drain_timeout.as_secs(),
        "workers did not drain within {timeout}s, killing",
        timeout = drain_timeout.as_secs(),
    );
    sigterm_then_sigkill(workers).await;
}

/// Phase 1: send Drain to every worker.
async fn send_drain_to_all(workers: &mut [WorkerHandle]) {
    for worker in workers.iter_mut() {
        if let Err(e) = worker.channel.send(&IpcMessage::Drain).await {
            tracing::debug!(name: "apx.supervisor.drain_send_failed", worker = worker.index, error = %e, "failed to send Drain");
        }
    }
}

/// Phase 2: wait for Drained IPC from every worker, returns `true` if all
/// drained within the timeout.
async fn wait_for_all_drained(workers: &mut [WorkerHandle], timeout: Duration) -> bool {
    let drain_all = async {
        for worker in workers.iter_mut() {
            match worker.channel.recv().await {
                Ok(IpcMessage::Drained) => {
                    tracing::info!(name: "apx.supervisor.drained", worker = worker.index, "worker drained");
                }
                Ok(msg) => {
                    tracing::debug!(name: "apx.supervisor.drain_unexpected_message", worker = worker.index, ?msg, "unexpected message during drain");
                }
                Err(e) => {
                    tracing::debug!(name: "apx.supervisor.drain_ipc_error", worker = worker.index, error = %e, "IPC error during drain");
                }
            }
        }
    };
    tokio::time::timeout(timeout, drain_all).await.is_ok()
}

/// Wait for all worker processes to exit, SIGKILL if they linger.
async fn wait_or_kill(workers: &mut [WorkerHandle]) {
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
        kill_all(workers).await;
    }
}

/// SIGTERM remaining workers, then SIGKILL after `SIGKILL_TIMEOUT`.
async fn sigterm_then_sigkill(workers: &mut [WorkerHandle]) {
    for worker in workers.iter() {
        if let Some(pid) = worker.child.id() {
            send_signal(pid, Signal::Term).await;
        }
    }

    let wait_all = async {
        for worker in workers.iter_mut() {
            let _ = worker.child.wait().await;
        }
    };
    if tokio::time::timeout(SIGKILL_TIMEOUT, wait_all)
        .await
        .is_err()
    {
        kill_all(workers).await;
    }
}

/// Send SIGKILL to all workers.
async fn kill_all(workers: &mut [WorkerHandle]) {
    for worker in workers.iter_mut() {
        let _ = worker.child.kill().await;
    }
}

/// Dev-reload shutdown: SIGTERM workers, wait for cleanup, SIGKILL stragglers.
///
/// Unlike `shutdown_workers` (production path), this skips IPC Drain entirely.
/// Workers receive SIGTERM, run Python lifespan cleanup, and exit.
pub(crate) async fn kill_workers(workers: &mut [WorkerHandle], cleanup_timeout: Duration) {
    for worker in workers.iter() {
        if let Some(pid) = worker.child.id() {
            send_signal(pid, Signal::Term).await;
        }
    }

    let wait_all = async {
        for worker in workers.iter_mut() {
            let _ = worker.child.wait().await;
        }
    };

    if tokio::time::timeout(cleanup_timeout, wait_all)
        .await
        .is_err()
    {
        tracing::warn!(
            name: "apx.supervisor.dev_reload_kill",
            timeout_secs = cleanup_timeout.as_secs(),
            "workers did not exit within cleanup timeout, sending SIGKILL",
        );
        kill_all(workers).await;
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

    fn test_config(workers: usize, port: u16) -> SupervisorConfig {
        SupervisorConfig {
            host: "127.0.0.1".to_owned(),
            port,
            workers,
            app_module: AppModule::new("backend.app").unwrap(),
            app_dir: PathBuf::from("/app"),
            request_timeout: Duration::from_secs(30),
            max_concurrent: None,
            loop_policy: "uvloop".to_owned(),
            dev_mode: false,
            drain_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn validate_config_valid() {
        assert!(validate_config(&test_config(4, 8000)).is_ok());
    }

    #[test]
    fn validate_config_dev_mode() {
        let mut config = test_config(1, 8000);
        config.dev_mode = true;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_config_zero_workers() {
        let err = validate_config(&test_config(0, 8000)).unwrap_err();
        assert!(matches!(
            err,
            SupervisorError::Config(ConfigError::ZeroWorkers(0))
        ));
    }

    #[test]
    fn validate_config_zero_port() {
        let err = validate_config(&test_config(4, 0)).unwrap_err();
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

    #[test]
    fn supervisor_error_display_worker_startup_failed() {
        let err = SupervisorError::WorkerStartupFailed {
            index: 0,
            error: "app load failed: no attribute 'app'".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("worker 0"));
        assert!(msg.contains("failed to start"));
        assert!(msg.contains("no attribute"));
    }
}
