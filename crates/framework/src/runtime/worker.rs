//! Single worker: initialize Python, bind TCP, serve requests.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter, one asyncio event loop, and one TCP listener bound via
//! `SO_REUSEPORT`.

use crate::app_loader::{AppSource, ModuleImport};
use crate::event_loop::EventLoop;
use crate::ipc::channel::WorkerChannel;
use crate::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use crate::service::{ApxService, ServiceConfig, serve_tcp};
use crate::signal::shutdown_signal;
use crate::transport::{Listener, TransportConfig, TransportError};
use crate::worker_context::WorkerContext;
use pyo3::Python;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// Errors during worker operation.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// TCP listener creation failed.
    #[error("transport: {0}")]
    Transport(#[from] TransportError),

    /// Python interpreter initialization failed.
    #[error("python init failed: {0}")]
    PythonInit(String),

    /// App loading failed (import, missing attribute, not callable).
    #[error("app load failed: {0}")]
    AppLoad(#[from] crate::app_loader::AppLoadError),

    /// IPC communication error.
    #[error("ipc: {0}")]
    Ipc(#[from] crate::ipc::protocol::IpcError),

    /// Serving requests failed.
    #[error("serve failed: {0}")]
    Serve(std::io::Error),
}

/// Phase 1 runtime: TCP listener + Python interpreter (expensive, survives reloads).
pub struct WorkerRuntime {
    /// TCP listener bound via the `Listener` trait.
    pub listener: crate::transport::TcpListener,
    /// IPC channel to supervisor — stays open for the worker's lifetime.
    pub channel: WorkerChannel,
    /// Persistent asyncio event loop on a dedicated thread.
    pub py_event_loop: EventLoop,
}

impl std::fmt::Debug for WorkerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerRuntime").finish_non_exhaustive()
    }
}

/// Phase 1: Create TCP listener and initialize the Python interpreter.
///
/// # Errors
///
/// Returns an error if the listener cannot be created or Python init fails.
pub async fn init_worker(
    bootstrap: &WorkerBootstrap,
    channel: WorkerChannel,
) -> Result<WorkerRuntime, WorkerError> {
    let host: IpAddr = bootstrap
        .host
        .parse()
        .map_err(|e| TransportError::InvalidHost {
            host: bootstrap.host.clone(),
            source: e,
        })?;
    let config = TransportConfig::tcp(host, bootstrap.port);
    let listener = crate::transport::TcpListener::bind(&config).await?;

    // Initialize the Python interpreter.
    // IMPORTANT: must only be called once per process, only in worker processes.
    Python::initialize();

    // Start a persistent asyncio event loop on a dedicated thread.
    let py_event_loop = EventLoop::start(&bootstrap.loop_policy)
        .map_err(|e| WorkerError::PythonInit(format!("event loop: {e}")))?;

    Ok(WorkerRuntime {
        listener,
        channel,
        py_event_loop,
    })
}

/// Signal readiness to supervisor over the IPC channel.
///
/// # Errors
///
/// Returns an error if the IPC send fails.
async fn signal_readiness(channel: &mut WorkerChannel) -> Result<(), WorkerError> {
    channel
        .send(&IpcMessage::Ready)
        .await
        .map_err(WorkerError::from)
}

/// Convenience: connect → init → signal readiness → load app → serve.
///
/// # Errors
///
/// Returns an error at any step in the worker lifecycle.
pub async fn run_worker(
    channel: WorkerChannel,
    bootstrap: WorkerBootstrap,
) -> Result<(), WorkerError> {
    let mut runtime = init_worker(&bootstrap, channel).await?;
    signal_readiness(&mut runtime.channel).await?;

    // Build WorkerContext from the event loop.
    let loop_handle = runtime.py_event_loop.handle();
    let event_loop_ref = Python::attach(|py| runtime.py_event_loop.event_loop_ref().clone_ref(py));
    let ctx = Arc::new(WorkerContext {
        loop_handle,
        event_loop_ref,
    });

    // Load app and build dispatch pipeline.
    let dispatch =
        Python::attach(|py| ModuleImport::new(bootstrap.app_module.as_str()).build(py, ctx))?;

    // Build HTTP service.
    let config = ServiceConfig {
        timeout: Duration::from_secs(bootstrap.request_timeout_secs),
        ..ServiceConfig::default()
    };
    let server_addr = runtime.listener.local_addr();
    let service = ApxService::new(dispatch, server_addr, &config);

    // Serve requests until shutdown.
    serve_tcp(runtime.listener, service, shutdown_signal())
        .await
        .map_err(WorkerError::Serve)?;

    // Flush pending OTLP spans, metrics, and logs before the event loop stops.
    apx_core::tracing_init::shutdown_telemetry();
    runtime.py_event_loop.stop();

    Ok(())
}

/// Detect worker mode and connect to the supervisor's IPC channel.
///
/// Returns `None` if `APX_WORKER_NONCE` is absent (supervisor mode).
/// Returns `Ok(Some(...))` if worker mode, with nonce verified.
///
/// # Errors
///
/// Returns `BootstrapError` on any failure in worker mode.
pub async fn connect_to_supervisor()
-> Result<Option<(WorkerChannel, WorkerBootstrap)>, BootstrapError> {
    let env_nonce_str = match std::env::var("APX_WORKER_NONCE") {
        Ok(val) => val,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(_) => return Err(BootstrapError::MissingNonce),
    };
    let env_nonce = Nonce::from_string(env_nonce_str);

    let sock_path = std::env::var("APX_WORKER_SOCK").map_err(|_| BootstrapError::MissingNonce)?;

    let mut channel = crate::ipc::channel::connect(&sock_path)
        .await
        .map_err(|e| BootstrapError::Connect {
            path: sock_path,
            source: std::io::Error::other(e.to_string()),
        })?;

    let msg = channel.recv().await.map_err(BootstrapError::from)?;
    let bootstrap = match msg {
        IpcMessage::Bootstrap(b) => b,
        other @ IpcMessage::Ready => {
            return Err(BootstrapError::UnexpectedMessage(format!("{other:?}")));
        }
    };

    if !env_nonce.verify(&bootstrap.nonce) {
        return Err(BootstrapError::NonceMismatch);
    }

    Ok(Some((channel, bootstrap)))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_error_display_python_init() {
        let err = WorkerError::PythonInit("failed".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("python init"));
    }

    #[test]
    fn worker_error_display_app_load() {
        let err = WorkerError::AppLoad(crate::app_loader::AppLoadError::MissingAttribute {
            module: "myapp".to_owned(),
            attr: "handler".to_owned(),
        });
        let msg = format!("{err}");
        assert!(msg.contains("app load"));
        assert!(msg.contains("no attribute"));
    }

    #[test]
    fn worker_error_display_transport() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let err = WorkerError::Transport(TransportError::Bind {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8000),
            source: std::io::Error::other("in use"),
        });
        let msg = format!("{err}");
        assert!(msg.contains("transport"));
    }
}
