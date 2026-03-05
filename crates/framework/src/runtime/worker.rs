//! Single worker: initialize Python, build router, serve requests.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter, one asyncio event loop, and one TCP listener bound via
//! `SO_REUSEPORT`.

use crate::bridge::dispatch::AppState;
use crate::bridge::{build_router, wrap_layers};
use crate::discovery;
use crate::ipc::channel::WorkerChannel;
use crate::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use crate::manifest::ManifestError;
use crate::runtime::lifecycle::LifecycleCache;
use crate::transport::{Listener, TransportConfig, TransportError};
use axum::Router;
use pyo3::Python;
use pyo3::types::PyAnyMethods;
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

    /// App discovery (import module, extract routes) failed.
    #[error("app discovery failed: {0}")]
    Discovery(#[from] discovery::DiscoveryError),

    /// IPC communication error.
    #[error("ipc: {0}")]
    Ipc(#[from] crate::ipc::protocol::IpcError),

    /// Manifest loading failed.
    #[error("manifest load: {0}")]
    ManifestLoad(#[from] ManifestError),

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

    // Set up asyncio event loop.
    Python::attach(|py| {
        let asyncio = py
            .import(c"asyncio")
            .map_err(|e| WorkerError::PythonInit(format!("import asyncio: {e}")))?;
        let event_loop = asyncio
            .call_method0(c"new_event_loop")
            .map_err(|e| WorkerError::PythonInit(format!("new_event_loop: {e}")))?;
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .map_err(|e| WorkerError::PythonInit(format!("set_event_loop: {e}")))?;
        Ok::<_, WorkerError>(())
    })?;

    Ok(WorkerRuntime { listener, channel })
}

/// Phase 2: Load the Python app and build the axum router.
///
/// Branches on `manifest_path`: when present, loads the pre-built manifest
/// and imports only handler functions (no FastAPI). Otherwise runs live
/// FastAPI discovery.
///
/// # Errors
///
/// Returns an error if discovery/manifest loading fails or the router can't be built.
pub fn load_app(
    bootstrap: &WorkerBootstrap,
    server_addr: std::net::SocketAddr,
) -> Result<(Router, Arc<AppState>), WorkerError> {
    match &bootstrap.manifest_path {
        Some(path) => load_from_manifest(path, server_addr),
        None => load_from_discovery(bootstrap, server_addr),
    }
}

/// Load routes from a pre-built manifest (no FastAPI import).
fn load_from_manifest(
    path: &std::path::Path,
    server_addr: std::net::SocketAddr,
) -> Result<(Router, Arc<AppState>), WorkerError> {
    let manifest = crate::manifest::load(path)?;
    crate::manifest::check_version(&manifest)?;

    let lifecycle_cache = Arc::new(LifecycleCache::empty());

    let routes = Python::attach(|py| discovery::bind::bind_routes_from_manifest(py, &manifest))?;

    let app_state = Arc::new(AppState {
        max_body_limit: manifest.max_body_limit,
    });

    let router = build_router(routes, Arc::clone(&app_state), server_addr, lifecycle_cache);
    Ok((router, app_state))
}

/// Load routes via live FastAPI discovery (dev mode).
fn load_from_discovery(
    bootstrap: &WorkerBootstrap,
    server_addr: std::net::SocketAddr,
) -> Result<(Router, Arc<AppState>), WorkerError> {
    Python::attach(|py| {
        let (routes, manifest) = discovery::discover_and_bind(py, &bootstrap.app_module)?;

        let app_state = Arc::new(AppState {
            max_body_limit: manifest.max_body_limit,
        });

        let lifecycle_cache = Arc::new(LifecycleCache::empty());
        let router = build_router(routes, Arc::clone(&app_state), server_addr, lifecycle_cache);
        Ok((router, app_state))
    })
}

/// Phase 3: Serve requests until shutdown.
///
/// Applies tower layers (CORS, trace, timeout, concurrency limit) and
/// serves with graceful shutdown via the `Listener` trait.
///
/// # Errors
///
/// Returns an error if the server fails to start.
pub async fn serve(
    listener: crate::transport::TcpListener,
    router: Router,
    request_timeout: Option<Duration>,
) -> Result<(), WorkerError> {
    let router = wrap_layers(router, request_timeout);
    listener
        .serve(router, shutdown_signal())
        .await
        .map_err(WorkerError::from)
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

/// Close the asyncio event loop gracefully.
fn shutdown_event_loop() {
    Python::attach(|py| {
        let _ = close_event_loop(py);
    });
}

/// Drain async generators and close the event loop.
fn close_event_loop(py: Python<'_>) -> pyo3::PyResult<()> {
    let asyncio = py.import(c"asyncio")?;
    let loop_obj = asyncio.call_method0(c"get_event_loop")?;
    let shutdown_coro = loop_obj.call_method0(c"shutdown_asyncgens")?;
    let _ = loop_obj.call_method1(c"run_until_complete", (shutdown_coro,));
    loop_obj.call_method0(c"close")?;
    Ok(())
}

/// Convenience: connect → init → load → signal readiness → serve → shutdown.
///
/// # Errors
///
/// Returns an error at any step in the worker lifecycle.
pub async fn run_worker(
    channel: WorkerChannel,
    bootstrap: WorkerBootstrap,
) -> Result<(), WorkerError> {
    let mut runtime = init_worker(&bootstrap, channel).await?;

    let server_addr = runtime.listener.local_addr();
    let (router, _app_state) = load_app(&bootstrap, server_addr)?;

    signal_readiness(&mut runtime.channel).await?;

    let timeout = if bootstrap.request_timeout_secs > 0 {
        Some(Duration::from_secs(bootstrap.request_timeout_secs))
    } else {
        None
    };

    let result = serve(runtime.listener, router, timeout).await;

    shutdown_event_loop();

    result
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

/// Re-export shared shutdown signal for worker use.
use crate::signal::shutdown_signal;

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_error_display_manifest_load() {
        let err = WorkerError::ManifestLoad(ManifestError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        )));
        let msg = format!("{err}");
        assert!(msg.contains("manifest load"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn worker_error_display_discovery() {
        let err =
            WorkerError::Discovery(discovery::DiscoveryError::NoApp("backend.app".to_owned()));
        let msg = format!("{err}");
        assert!(msg.contains("discovery"));
    }

    #[test]
    fn worker_error_display_python_init() {
        let err = WorkerError::PythonInit("failed".to_owned());
        let msg = format!("{err}");
        assert!(msg.contains("python init"));
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

    #[test]
    fn worker_error_manifest_load_version_mismatch() {
        let err = WorkerError::ManifestLoad(ManifestError::VersionMismatch {
            manifest: "0.1.0".to_owned(),
            running: "0.2.0".to_owned(),
        });
        let msg = format!("{err}");
        assert!(msg.contains("version mismatch"));
    }
}
