//! Single worker: initialize Python, serve requests via asyncio.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter with an asyncio event loop. TCP binding happens via asyncio's
//! `loop.create_server()` with `SO_REUSEPORT`.

use super::ipc::channel::WorkerChannel;
use super::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use super::signal::shutdown_signal;
use crate::asgi::app::{ModuleImport, format_pyerr};
use crate::asgi::scope::ScopeInterns;
use crate::protocol::connection::ProtocolFactory;
use pyo3::prelude::*;
use std::net::{IpAddr, SocketAddr};

/// Errors during worker operation.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// TCP listener creation failed.
    #[error("transport: {0}")]
    Transport(#[from] crate::transport::TransportError),

    /// Python interpreter initialization failed.
    #[error("python init failed: {0}")]
    PythonInit(String),

    /// App loading failed (import, missing attribute, not callable).
    #[error("app load failed: {0}")]
    AppLoad(#[from] crate::asgi::app::AppLoadError),

    /// IPC communication error.
    #[error("ipc: {0}")]
    Ipc(#[from] super::ipc::protocol::IpcError),

    /// Serving requests failed.
    #[error("serve failed: {0}")]
    Serve(String),

    /// ASGI lifespan startup failed.
    #[error("lifespan startup failed: {0}")]
    LifespanStartup(String),
}

/// Format a worker error with full Python traceback when available.
pub fn format_worker_error(err: &WorkerError) -> String {
    match err {
        WorkerError::AppLoad(crate::asgi::app::AppLoadError::ImportFailed { source, .. }) => {
            Python::attach(|py| format_pyerr(py, source))
        }
        WorkerError::LifespanStartup(msg) => format!("lifespan startup failed: {msg}"),
        _ => err.to_string(),
    }
}

/// Loaded app state, ready to serve.
struct AppReady {
    /// ASGI application callable.
    asgi_app: Py<PyAny>,
    /// Pre-built scope interns for the server address.
    interns: ScopeInterns,
    /// Telemetry configuration.
    telemetry: crate::telemetry::config::TelemetryConfig,
    /// Server socket address.
    server_addr: SocketAddr,
}

crate::opaque_debug!(AppReady);

/// Initialize the Python interpreter.
fn init_python() {
    Python::initialize();
}

/// Apply the asyncio event loop policy before the loop is created.
///
/// uvloop provides ~5-10x faster transport.write() and selector dispatch
/// compared to the default `_UnixSelectorEventLoop`.
fn install_loop_policy(
    py: Python<'_>,
    asyncio: &Bound<'_, PyModule>,
    loop_policy: &str,
) -> Result<(), WorkerError> {
    if loop_policy == "uvloop" {
        match py.import(c"uvloop") {
            Ok(uvloop) => {
                let policy = uvloop
                    .call_method0(c"EventLoopPolicy")
                    .map_err(|e| WorkerError::Serve(format!("uvloop.EventLoopPolicy: {e}")))?;
                asyncio
                    .call_method1(c"set_event_loop_policy", (policy,))
                    .map_err(|e| WorkerError::Serve(format!("set_event_loop_policy: {e}")))?;
                tracing::info!(
                    name: "apx.worker.loop_policy",
                    policy = "uvloop",
                    "event loop policy set"
                );
            }
            Err(_) => {
                tracing::warn!(
                    name: "apx.worker.loop_policy_fallback",
                    requested = "uvloop",
                    fallback = "asyncio",
                    "uvloop not available, falling back to default asyncio"
                );
            }
        }
    } else {
        tracing::info!(
            name: "apx.worker.loop_policy",
            policy = loop_policy,
            "using default asyncio event loop"
        );
    }
    Ok(())
}

/// Load the Python app and read telemetry configuration.
fn load_app(bootstrap: &WorkerBootstrap) -> Result<AppReady, WorkerError> {
    apply_python_log_config()?;

    let host: IpAddr =
        bootstrap
            .host
            .parse()
            .map_err(|e| crate::transport::TransportError::InvalidHost {
                host: bootstrap.host.clone(),
                source: e,
            })?;
    let server_addr = SocketAddr::new(host, bootstrap.port);

    let (asgi_app, interns) =
        Python::attach(|py| -> Result<(Py<PyAny>, ScopeInterns), WorkerError> {
            let app_import = ModuleImport::new(bootstrap.app_module.as_str());
            let app = app_import.load_callable(py).map_err(WorkerError::AppLoad)?;
            let asgi_app = app.inner().clone_ref(py);
            let interns = ScopeInterns::new(py, server_addr);
            Ok((asgi_app, interns))
        })?;

    let telemetry = Python::attach(|py| {
        crate::telemetry::bootstrap_python_telemetry(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry bootstrap: {e}")))?;
        crate::telemetry::config::read_python_config(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry config: {e}")))
    })?;

    Ok(AppReady {
        asgi_app,
        interns,
        telemetry,
        server_addr,
    })
}

/// Signal readiness to supervisor over the IPC channel.
async fn signal_readiness(channel: &mut WorkerChannel) -> Result<(), WorkerError> {
    channel
        .send(&IpcMessage::Ready)
        .await
        .map_err(WorkerError::from)
}

/// Relay telemetry config to the supervisor (worker 0 only).
async fn relay_telemetry(
    channel: &mut WorkerChannel,
    bootstrap: &WorkerBootstrap,
    telemetry: &crate::telemetry::config::TelemetryConfig,
) -> Result<(), WorkerError> {
    if !bootstrap.relay_telemetry {
        return Ok(());
    }
    let relay = super::ipc::protocol::TelemetryRelay {
        system: telemetry.system,
        process: telemetry.process,
    };
    channel
        .send(&IpcMessage::TelemetryConfig(relay))
        .await
        .map_err(WorkerError::from)?;
    tracing::debug!(
        name: "apx.worker.telemetry_relayed",
        target: "apx::telemetry",
        "relayed telemetry config to supervisor"
    );
    Ok(())
}

/// Initialize per-worker metric toggles and spawn collectors.
fn init_metrics(telemetry: &crate::telemetry::config::TelemetryConfig) {
    crate::telemetry::http::init(telemetry.http.metrics);
    crate::telemetry::dispatch_metrics::init(telemetry.apx.metrics);

    tracing::debug!(
        name: "apx.worker.telemetry_bootstrap_complete",
        target: "apx::telemetry",
        process_metrics = telemetry.process.enabled,
        http_instrumentation = telemetry.http.enabled,
        apx_dispatch_metrics = telemetry.apx.enabled,
        otel_endpoint = %std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default(),
        meter_provider = apx_core::tracing_init::meter_provider().is_some(),
        "telemetry bootstrap complete"
    );
}

/// Run the asyncio server via `asyncio.run(serve(...))`.
///
/// The asyncio event loop owns everything: TCP accept, HTTP parsing,
/// request dispatch, and response writing. Rust provides accelerated
/// primitives as PyO3 `#[pyclass]` types.
fn run_server(
    ready: AppReady,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    loop_policy: &str,
) -> Result<(), WorkerError> {
    Python::attach(|py| {
        let asyncio = py
            .import(c"asyncio")
            .map_err(|e| WorkerError::Serve(format!("import asyncio: {e}")))?;

        // Apply the event loop policy before asyncio.run() creates the loop.
        // uvloop is ~5-10x faster than the default selector loop for
        // transport.write() and selector dispatch.
        install_loop_policy(py, &asyncio, loop_policy)?;

        let shutdown_event = asyncio
            .call_method0(c"Event")
            .map_err(|e| WorkerError::Serve(format!("create Event: {e}")))?;

        let host = ready.server_addr.ip().to_string();
        let port = ready.server_addr.port();

        let factory_builder =
            create_factory_builder(py, ready.asgi_app.clone_ref(py), ready.interns, &host, port)?;

        py.run(
            c"
import asyncio as _asyncio
from apx._server import serve as _serve, _build_on_request
from apx._scheduler import CallSoonCapture

async def _boot(_app, _factory_fn, _host, _port, _shutdown_event):
    loop = _asyncio.get_running_loop()
    capture = CallSoonCapture(loop)
    on_request = _build_on_request(_app, loop, capture)
    factory = _factory_fn(on_request)
    await _serve(_host, _port, _app, factory, shutdown_event=_shutdown_event)
",
            None,
            None,
        )
        .map_err(|e| WorkerError::Serve(format!("compile bootstrap: {e}")))?;

        let boot_fn = py
            .eval(c"_boot", None, None)
            .map_err(|e| WorkerError::Serve(format!("get _boot: {e}")))?;

        let coro = boot_fn
            .call1((
                &ready.asgi_app,
                factory_builder,
                &host,
                port,
                &shutdown_event,
            ))
            .map_err(|e| WorkerError::Serve(format!("create boot coro: {e}")))?;

        let shutdown_event_ref = shutdown_event.unbind();
        std::thread::spawn(move || {
            let _ = shutdown_rx.blocking_recv();
            Python::attach(|py| {
                let _ = shutdown_event_ref.call_method0(py, pyo3::intern!(py, "set"));
            });
        });

        asyncio
            .call_method1(c"run", (coro,))
            .map_err(|e| WorkerError::Serve(format!("asyncio.run: {e}")))?;

        Ok(())
    })
}

/// Create a Python callable that, given `on_request`, returns a `ProtocolFactory`.
///
/// This is a partial application: `ScopeInterns`, host, and port are
/// captured; the `on_request` callback is provided later (once the
/// event loop is running and `CallSoonCapture` can be created).
fn create_factory_builder(
    py: Python<'_>,
    _app: Py<PyAny>,
    interns: ScopeInterns,
    host: &str,
    port: u16,
) -> Result<Py<PyAny>, WorkerError> {
    let host = host.to_owned();

    let builder = FactoryBuilder {
        interns: std::sync::Mutex::new(Some(interns)),
        host,
        port,
    };
    let builder_py = Py::new(py, builder)
        .map_err(|e| WorkerError::Serve(format!("create FactoryBuilder: {e}")))?;

    Ok(builder_py.into_any())
}

/// Python-callable that captures `ScopeInterns` and produces a
/// `ProtocolFactory` when called with `on_request`.
#[pyclass(module = "apx._core")]
struct FactoryBuilder {
    interns: std::sync::Mutex<Option<ScopeInterns>>,
    host: String,
    port: u16,
}

crate::opaque_debug!(FactoryBuilder);

#[pymethods]
impl FactoryBuilder {
    fn __call__(&self, py: Python<'_>, on_request: Py<PyAny>) -> PyResult<Py<ProtocolFactory>> {
        let interns = self
            .interns
            .lock()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
            .take()
            .ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("FactoryBuilder already consumed")
            })?;
        let factory = ProtocolFactory::new(on_request, interns, self.host.clone(), self.port);
        Py::new(py, factory)
    }
}

/// Connect, init, load app, signal readiness, and serve.
///
/// # Errors
///
/// Returns an error at any step in the worker lifecycle.
pub async fn run_worker(
    channel: WorkerChannel,
    bootstrap: WorkerBootstrap,
) -> Result<(), WorkerError> {
    init_python();

    let ready = match load_app(&bootstrap) {
        Ok(ready) => ready,
        Err(e) => {
            let detail = format_worker_error(&e);
            let mut ch = channel;
            let _ = ch.send(&IpcMessage::StartupFailed { error: detail }).await;
            return Err(e);
        }
    };

    let mut channel = channel;
    signal_readiness(&mut channel).await?;
    relay_telemetry(&mut channel, &bootstrap, &ready.telemetry).await?;
    init_metrics(&ready.telemetry);

    if ready.telemetry.process.enabled {
        crate::telemetry::process_metrics::register_process_metrics(
            ready.telemetry.process.metrics,
        );
    }

    // Set up shutdown coordination between IPC reader and asyncio.
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let (ipc_reader, mut ipc_writer) = channel.split();

    tokio::spawn(async move {
        let mut reader = ipc_reader;
        tokio::select! {
            msg = reader.recv() => {
                match msg {
                    Ok(IpcMessage::Drain) => {
                        tracing::info!(
                            name: "apx.worker.drain_received",
                            "received Drain from supervisor"
                        );
                        let _ = drain_tx.send(());
                    }
                    Ok(msg) => tracing::warn!(
                        name: "apx.worker.drain_unexpected_ipc",
                        ?msg,
                        "unexpected IPC message"
                    ),
                    Err(e) => tracing::debug!(
                        name: "apx.worker.drain_ipc_closed",
                        error = %e,
                        "IPC channel closed"
                    ),
                }
            }
            () = shutdown_signal() => {
                let _ = drain_tx.send(());
            }
        }
    });

    // Run the asyncio server (blocking — this IS the event loop).
    let loop_policy = bootstrap.loop_policy.clone();
    let serve_result =
        tokio::task::spawn_blocking(move || run_server(ready, drain_rx, &loop_policy))
            .await
            .map_err(|e| WorkerError::Serve(format!("server task panicked: {e}")))?;

    let _ = ipc_writer.send(&IpcMessage::Drained).await;
    apx_core::tracing_init::shutdown_telemetry();

    serve_result
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

    let mut channel = super::ipc::channel::connect(&sock_path)
        .await
        .map_err(|e| BootstrapError::Connect {
            path: sock_path,
            source: std::io::Error::other(e.to_string()),
        })?;

    let msg = channel.recv().await.map_err(BootstrapError::from)?;
    let bootstrap = match msg {
        IpcMessage::Bootstrap(b) => b,
        other => {
            return Err(BootstrapError::UnexpectedMessage(format!("{other:?}")));
        }
    };

    if !env_nonce.verify(&bootstrap.nonce) {
        return Err(BootstrapError::NonceMismatch);
    }

    Ok(Some((channel, bootstrap)))
}

// ── Python logging config ───────────────────────────────────────────────

/// Apply the customer's Python logging config when `APX_PYTHON_LOG_CONFIG`
/// is set.
fn apply_python_log_config() -> Result<(), WorkerError> {
    let config_path = match std::env::var("APX_PYTHON_LOG_CONFIG") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()),
    };

    Python::attach(|py| {
        let locals = pyo3::types::PyDict::new(py);
        locals.set_item("_path", &config_path)?;

        let path = std::path::Path::new(&config_path);
        if path.extension().is_some_and(|ext| ext == "py") {
            py.run(
                c"import logging.config; logging.config.fileConfig(_path)",
                None,
                Some(&locals),
            )?;
        } else {
            py.run(
                c"import json, logging.config, pathlib; logging.config.dictConfig(json.loads(pathlib.Path(_path).read_text()))",
                None,
                Some(&locals),
            )?;
        }
        Ok(())
    })
    .map_err(|e: PyErr| WorkerError::PythonInit(format!("log config: {e}")))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
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
        let err = WorkerError::AppLoad(crate::asgi::app::AppLoadError::MissingAttribute {
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
        let err = WorkerError::Transport(crate::transport::TransportError::Bind {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8000),
            source: std::io::Error::other("in use"),
        });
        let msg = format!("{err}");
        assert!(msg.contains("transport"));
    }

    // SAFETY: these env-var tests are single-threaded (#[test] with no async
    // or parallel spawns), so set_var / remove_var cannot race.
    #[expect(unsafe_code, reason = "env-var manipulation in single-threaded test")]
    #[test]
    fn apply_log_config_noop_when_unset() {
        unsafe { std::env::remove_var("APX_PYTHON_LOG_CONFIG") };
        apply_python_log_config().expect("noop when env var absent");
    }

    #[expect(unsafe_code, reason = "env-var manipulation in single-threaded test")]
    #[test]
    fn apply_log_config_noop_when_empty() {
        unsafe { std::env::set_var("APX_PYTHON_LOG_CONFIG", "") };
        let result = apply_python_log_config();
        unsafe { std::env::remove_var("APX_PYTHON_LOG_CONFIG") };
        result.expect("noop when env var empty");
    }

    #[expect(unsafe_code, reason = "env-var manipulation in single-threaded test")]
    #[test]
    fn apply_log_config_json() {
        crate::with_py(|_py| {
            let dir = tempfile::tempdir().expect("tmpdir");
            let config_path = dir.path().join("logging.json");
            std::fs::write(
                &config_path,
                r#"{"version": 1, "disable_existing_loggers": false, "handlers": {}, "loggers": {}}"#,
            )
            .expect("write config");

            unsafe {
                std::env::set_var("APX_PYTHON_LOG_CONFIG", config_path.to_str().expect("utf8"));
            }
            let result = apply_python_log_config();
            unsafe { std::env::remove_var("APX_PYTHON_LOG_CONFIG") };
            result.expect("dictConfig should succeed");
        });
    }
}
