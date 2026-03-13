//! Single worker: initialize Python, build router, serve requests.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter, one asyncio event loop, and one TCP listener bound via
//! `SO_REUSEPORT`.

use crate::bridge::dispatch::AppState;
use crate::bridge::{build_router, wrap_layers};
use crate::discovery;
use crate::event_loop::EventLoop;
use crate::event_loop::scheduling::AsgiErrorLogger;
use crate::ipc::channel::WorkerChannel;
use crate::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use crate::manifest::ManifestError;
use crate::runtime::lifecycle::{LifecycleCache, LifecycleError};
use crate::transport::{Listener, TransportConfig, TransportError};
use axum::Router;
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

    /// App discovery (import module, extract routes) failed.
    #[error("app discovery failed: {0}")]
    Discovery(#[from] discovery::DiscoveryError),

    /// IPC communication error.
    #[error("ipc: {0}")]
    Ipc(#[from] crate::ipc::protocol::IpcError),

    /// Manifest loading failed.
    #[error("manifest load: {0}")]
    ManifestLoad(#[from] ManifestError),

    /// Lifecycle dependency initialization failed.
    #[error("lifecycle init: {0}")]
    Lifecycle(#[from] LifecycleError),

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
    // Handler coroutines are submitted via call_soon_threadsafe.
    let py_event_loop =
        EventLoop::start().map_err(|e| WorkerError::PythonInit(format!("event loop: {e}")))?;

    Ok(WorkerRuntime {
        listener,
        channel,
        py_event_loop,
    })
}

/// Phase 2: Load the Python app from the manifest and build the axum router.
///
/// When `manifest_path` is `Some`, loads the pre-built manifest from disk
/// and validates it (the `apx build` + `apx serve <manifest>` path).
/// When `None`, calls `apx._manifest.compile_manifest()` in the embedded
/// interpreter to extract routes from the live app (the `apx serve <module>` path).
///
/// # Errors
///
/// Returns an error if manifest loading/validation fails or the router can't be built.
pub fn load_app(
    bootstrap: &WorkerBootstrap,
    server_addr: std::net::SocketAddr,
    py_event_loop: &EventLoop,
) -> Result<(Router, Arc<AppState>, Arc<LifecycleCache>), WorkerError> {
    let loop_handle = py_event_loop
        .handle()
        .map_err(|e| WorkerError::PythonInit(format!("event loop handle: {e}")))?;

    let (manifest, app_module) = if let Some(ref path) = bootstrap.manifest_path {
        // Manifest-based path (apx build + apx serve <manifest>)
        let manifest = crate::manifest::load(path)?;
        let meta = crate::manifest::validate_for_serving(&manifest)?;
        let app_mod = meta.app_module.clone();
        (manifest, app_mod)
    } else {
        // Live-import path (apx serve <app_module>)
        let manifest = Python::attach(|py| {
            discovery::fastapi::live_extract_manifest(py, &bootstrap.app_module)
        })?;
        (manifest, bootstrap.app_module.clone())
    };

    let (lifecycle_cache, routes, scope_interns) = Python::attach(|py| {
        // Bootstrap Python telemetry (log handler + context var) before app code runs.
        crate::telemetry::bootstrap_python_telemetry(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry bootstrap: {e}")))?;

        let cache = LifecycleCache::initialize(py, &manifest.lifecycle_deps)?;
        let routes = discovery::bind::bind_routes_from_manifest(py, &manifest, &app_module)?;
        let interns = crate::bridge::asgi::ScopeInterns::new(py);
        Ok::<_, WorkerError>((cache, routes, interns))
    })?;
    let lifecycle_cache = Arc::new(lifecycle_cache);

    let scope_interns = Arc::new(scope_interns);

    // Build scope template with fixed ASGI fields.
    // Use the FastAPI app from the first route (all routes share the same app).
    let scope_template = Python::attach(|py| {
        let fastapi_app = routes.iter().find_map(|r| r.fastapi_app.as_ref());
        crate::bridge::context_pool::build_scope_template(
            py,
            &scope_interns,
            fastapi_app.map(|a| a.inner()),
            server_addr,
        )
    })
    .map_err(|e| WorkerError::PythonInit(format!("scope template: {e}")))?;

    // Build receive template with fixed ASGI fields.
    let receive_template = Python::attach(crate::bridge::context_pool::build_receive_template)
        .map_err(|e| WorkerError::PythonInit(format!("receive template: {e}")))?;

    let (create_task, error_logger) = Python::attach(|py| {
        let create_task = py_event_loop
            .event_loop_ref()
            .getattr(py, "create_task")
            .map_err(|e| WorkerError::PythonInit(format!("create_task: {e}")))?;

        // Singleton error logger — stateless, reused across all requests.
        let error_logger = pyo3::Py::new(py, AsgiErrorLogger)
            .map_err(|e| WorkerError::PythonInit(format!("error logger: {e}")))?
            .into_any();

        Ok::<_, WorkerError>((create_task, error_logger))
    })?;

    let scheduler_refs = py_event_loop
        .scheduler_refs()
        .map(|refs| Arc::new(refs.clone()));

    let app_state = Arc::new(AppState {
        max_body_limit: manifest.max_body_limit,
        loop_handle,
        scope_interns,
        scope_template: Arc::new(scope_template),
        receive_template: Arc::new(receive_template),
        create_task,
        error_logger,
        scheduler_refs,
    });

    let router = build_router(routes, Arc::clone(&app_state), server_addr);
    Ok((router, app_state, lifecycle_cache))
}

/// Phase 3: Serve requests until shutdown.
///
/// Applies tower layers (trace, timeout, concurrency limit) and
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
    let (router, _app_state, lifecycle_cache) =
        load_app(&bootstrap, server_addr, &runtime.py_event_loop)?;

    signal_readiness(&mut runtime.channel).await?;

    let timeout = if bootstrap.request_timeout_secs > 0 {
        Some(Duration::from_secs(bootstrap.request_timeout_secs))
    } else {
        None
    };

    let result = serve(runtime.listener, router, timeout).await;

    Python::attach(|py| lifecycle_cache.shutdown(py));
    // Flush pending OTLP spans, metrics, and logs before the event loop stops.
    apx_core::tracing_init::shutdown_telemetry();
    // EventLoop::stop() is called by Drop, but we call explicitly for clarity.
    runtime.py_event_loop.stop();

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

    #[test]
    fn worker_error_display_lifecycle() {
        let err = WorkerError::Lifecycle(LifecycleError::Init {
            qualname: "db.engine".to_owned(),
            message: "connection refused".to_owned(),
        });
        let msg = format!("{err}");
        assert!(msg.contains("lifecycle init"));
    }
}
