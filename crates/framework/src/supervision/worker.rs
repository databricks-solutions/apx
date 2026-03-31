//! Single worker: initialize Python, bind TCP, serve requests.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter, one inline asyncio event loop, and one TCP listener bound via
//! `SO_REUSEPORT`.

use super::ipc::channel::WorkerChannel;
use super::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use super::signal::shutdown_signal;
use super::worker_context::WorkerContext;
use crate::asgi::app::{AppSource, ModuleImport, format_pyerr};
use crate::io::EventLoop;
use crate::protocol::http::service::{ApxService, ServiceConfig, serve_tcp};
use crate::transport::{Listener, TransportConfig, TransportError};
use pyo3::prelude::*;
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
    AppLoad(#[from] crate::asgi::app::AppLoadError),

    /// IPC communication error.
    #[error("ipc: {0}")]
    Ipc(#[from] super::ipc::protocol::IpcError),

    /// Serving requests failed.
    #[error("serve failed: {0}")]
    Serve(std::io::Error),
}

/// Format a worker error with full Python traceback when available.
///
/// Pattern-matches through the error chain to find a `PyErr` inside
/// `AppLoadError::ImportFailed` and renders its traceback. Falls back to
/// the standard `Display` chain for non-Python errors.
pub fn format_worker_error(err: &WorkerError) -> String {
    if let WorkerError::AppLoad(crate::asgi::app::AppLoadError::ImportFailed { source, .. }) = err {
        return Python::attach(|py| format_pyerr(py, source));
    }
    err.to_string()
}

/// Phase 1 runtime: TCP listener + Python interpreter (expensive, survives reloads).
pub struct WorkerRuntime {
    /// TCP listener bound via the `Listener` trait.
    pub listener: crate::transport::TcpListener,
    /// IPC channel to supervisor — stays open for the worker's lifetime.
    pub channel: WorkerChannel,
    /// Asyncio event loop (dedicated thread, asyncio delegation).
    pub event_loop: EventLoop,
}

crate::opaque_debug!(WorkerRuntime);

/// Phase 1: Create TCP listener and initialize the Python interpreter.
///
/// Uses `io::EventLoop` — creates the asyncio loop on a dedicated thread.
/// Coroutines are submitted via `call_soon_threadsafe(create_task, coro)`.
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

    // Initialize asyncio event loop (dedicated thread, asyncio delegation).
    let event_loop = Python::attach(|py| EventLoop::init(py, &bootstrap.loop_policy))
        .map_err(|e| WorkerError::PythonInit(format!("event loop: {e}")))?;

    Ok(WorkerRuntime {
        listener,
        channel,
        event_loop,
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

/// Loaded app and telemetry config, ready for readiness signal.
struct AppReady {
    dispatch: Arc<dyn crate::dispatch::Dispatch>,
    telemetry: crate::telemetry::config::TelemetryConfig,
}

crate::opaque_debug!(AppReady);

/// Load the Python app and read telemetry configuration.
///
/// Covers every fallible step between `init_worker` and `signal_readiness`.
/// On failure the caller sends `StartupFailed` over IPC before exiting.
fn load_app(runtime: &WorkerRuntime, bootstrap: &WorkerBootstrap) -> Result<AppReady, WorkerError> {
    apply_python_log_config()?;

    let pipeline = Arc::new(
        crate::io::channel::DispatchPipeline::new()
            .map_err(|e| WorkerError::PythonInit(format!("dispatch pipeline: {e}")))?,
    );

    let ctx = {
        let el = &runtime.event_loop;
        Python::attach(|py| -> Result<Arc<WorkerContext>, WorkerError> {
            let launch_fn = register_launch(py)
                .map_err(|e| WorkerError::PythonInit(format!("register launch: {e}")))?;
            Ok(Arc::new(WorkerContext {
                pipeline: Arc::clone(&pipeline),
                call_soon_threadsafe: el.call_soon_threadsafe().clone_ref(py),
                launch_fn,
            }))
        })?
    };

    let server_addr = runtime.listener.local_addr();
    let event_loop_py = runtime.event_loop.event_loop_py();
    let dispatch = Python::attach(|py| {
        ModuleImport::new(bootstrap.app_module.as_str(), bootstrap.dev_mode).build(
            py,
            ctx,
            event_loop_py,
            server_addr,
        )
    })?;

    let telemetry = Python::attach(|py| {
        crate::telemetry::bootstrap_python_telemetry(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry bootstrap: {e}")))?;
        crate::telemetry::config::read_python_config(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry config: {e}")))
    })?;

    Ok(AppReady {
        dispatch,
        telemetry,
    })
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

/// Build the HTTP service from dispatch and bootstrap config.
fn build_service(
    runtime: &WorkerRuntime,
    bootstrap: &WorkerBootstrap,
    dispatch: Arc<dyn crate::dispatch::Dispatch>,
) -> ApxService {
    let mut config = ServiceConfig {
        timeout: Duration::from_secs(bootstrap.request_timeout_secs),
        ..ServiceConfig::default()
    };
    if let Some(mc) = bootstrap.max_concurrent {
        config.max_concurrent = mc;
    }
    let server_addr = runtime.listener.local_addr();
    ApxService::new(dispatch, server_addr, &config)
}

/// Accept connections and serve until shutdown or drain.
async fn serve(
    runtime: WorkerRuntime,
    service: ApxService,
    drain_timeout_secs: u64,
) -> Result<(), WorkerError> {
    let (ipc_reader, mut ipc_writer) = runtime.channel.split();

    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut reader = ipc_reader;
        match reader.recv().await {
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
    });

    let combined_shutdown = async {
        tokio::select! {
            () = shutdown_signal() => {}
            _ = drain_rx => {}
        }
    };

    let mut connections = serve_tcp(runtime.listener, service, combined_shutdown)
        .await
        .map_err(WorkerError::Serve)?;

    if drain_timeout_secs > 0 {
        let _ = tokio::time::timeout(Duration::from_secs(drain_timeout_secs), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
    }

    let _ = ipc_writer.send(&IpcMessage::Drained).await;

    apx_core::tracing_init::shutdown_telemetry();
    runtime.event_loop.shutdown();

    Ok(())
}

/// Connect, init, load app, signal readiness, and serve.
///
/// If app loading fails, sends `StartupFailed` over IPC so the supervisor
/// receives the error message instead of waiting for a readiness timeout.
///
/// # Errors
///
/// Returns an error at any step in the worker lifecycle.
pub async fn run_worker(
    channel: WorkerChannel,
    bootstrap: WorkerBootstrap,
) -> Result<(), WorkerError> {
    let mut runtime = init_worker(&bootstrap, channel).await?;

    let ready = match load_app(&runtime, &bootstrap) {
        Ok(ready) => ready,
        Err(e) => {
            let detail = format_worker_error(&e);
            let _ = runtime
                .channel
                .send(&IpcMessage::StartupFailed { error: detail })
                .await;
            return Err(e);
        }
    };

    signal_readiness(&mut runtime.channel).await?;
    relay_telemetry(&mut runtime.channel, &bootstrap, &ready.telemetry).await?;
    init_metrics(&ready.telemetry);

    if ready.telemetry.process.enabled {
        crate::telemetry::process_metrics::register_process_metrics(
            ready.telemetry.process.metrics,
        );
    }

    let service = build_service(&runtime, &bootstrap, ready.dispatch);
    serve(runtime, service, bootstrap.drain_timeout_secs).await
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
/// is set (dev mode only).  Supports JSON (`logging.config.dictConfig`) and
/// Python (`logging.config.fileConfig`) config files.
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

// ── launch wrapper ──────────────────────────────────────────────────────

/// Import `launch` from `apx._bridge`.
///
/// `launch(app, scope, receive, send)` runs on the asyncio thread as a
/// `call_soon_threadsafe` callback. It calls `app(scope, receive, send)`
/// and wraps the coroutine in error-guarding + `create_task` — all in a
/// single `_run_once` callback, keeping the tokio thread GIL-free.
fn register_launch(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let bridge = py.import(c"apx._bridge")?;
    bridge.getattr(c"launch").map(|f| f.unbind())
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
        let err = WorkerError::Transport(TransportError::Bind {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8000),
            source: std::io::Error::other("in use"),
        });
        let msg = format!("{err}");
        assert!(msg.contains("transport"));
    }

    /// `launch` must forward app exceptions through `send.send_error()`
    /// without re-raising — otherwise asyncio logs "Task exception was
    /// never retrieved" on every app error (the task is fire-and-forget).
    #[test]
    fn launch_forwards_error_without_asyncio_leak() {
        crate::with_py(|py| {
            let launch_fn = register_launch(py).expect("register_launch");

            py.run(
                c"
import asyncio, gc

_leak_errors = []

def _capture(loop, ctx):
    _leak_errors.append(ctx.get('message', ''))

class _MockSend:
    def __init__(self):
        self.errors = []
    def send_error(self, tb):
        self.errors.append(tb)

_mock = _MockSend()

async def _failing_app(scope, receive, send):
    raise RuntimeError('deliberate test error')

_el = asyncio.new_event_loop()
_el.set_exception_handler(_capture)
",
                None,
                None,
            )
            .expect("define fixtures");

            let app = py.eval(c"_failing_app", None, None).expect("get app");
            let mock = py.eval(c"_mock", None, None).expect("get mock");
            let scope = pyo3::types::PyDict::new(py);
            let el = py.eval(c"_el", None, None).expect("get el");

            // Submit via call_soon_threadsafe — same as production.
            let csts = el.getattr(c"call_soon_threadsafe").expect("csts");
            csts.call1((&launch_fn, &app, &scope, py.None(), &mock))
                .expect("submit via csTS");

            py.run(
                c"
async def _drain():
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    gc.collect()
    gc.collect()
    await asyncio.sleep(0)

_el.run_until_complete(_drain())
_el.close()
",
                None,
                None,
            )
            .expect("drain loop");

            let send_errors: Vec<String> = py
                .eval(c"_mock.errors", None, None)
                .expect("get send_errors")
                .extract()
                .expect("extract");
            assert!(
                !send_errors.is_empty(),
                "send_error must be called on app exception"
            );
            assert!(
                send_errors[0].contains("deliberate test error"),
                "traceback must contain the error: {}",
                send_errors[0]
            );

            let leaks: Vec<String> = py
                .eval(c"_leak_errors", None, None)
                .expect("get leak errors")
                .extract()
                .expect("extract");
            let task_leaks: Vec<_> = leaks
                .iter()
                .filter(|e| e.contains("Task exception was never retrieved"))
                .collect();
            assert!(
                task_leaks.is_empty(),
                "launch re-raised, causing asyncio log spam: {task_leaks:?}"
            );
        });
    }

    /// `CancelledError` must propagate through `launch` — it's a control
    /// flow signal, not an app error. It must NOT be forwarded to
    /// `send.send_error()`.
    #[test]
    fn launch_propagates_cancellation() {
        crate::with_py(|py| {
            let launch_fn = register_launch(py).expect("register_launch");

            py.run(
                c"
import asyncio

class _MockSend2:
    def __init__(self):
        self.errors = []
    def send_error(self, tb):
        self.errors.append(tb)

_mock2 = _MockSend2()

async def _slow_app(scope, receive, send):
    await asyncio.sleep(10)

_el2 = asyncio.new_event_loop()
",
                None,
                None,
            )
            .expect("define fixtures");

            let app = py.eval(c"_slow_app", None, None).expect("get app");
            let mock = py.eval(c"_mock2", None, None).expect("get mock");
            let scope = pyo3::types::PyDict::new(py);
            let el = py.eval(c"_el2", None, None).expect("get el");

            let csts = el.getattr(c"call_soon_threadsafe").expect("csts");
            csts.call1((&launch_fn, &app, &scope, py.None(), &mock))
                .expect("submit via csTS");

            py.run(
                c"
async def _run():
    await asyncio.sleep(0)  # let launch create the task
    # Find the app task (not ourselves).
    app_tasks = [t for t in asyncio.all_tasks(_el2)
                 if not t.done() and t is not asyncio.current_task()]
    for t in app_tasks:
        t.cancel()
    # Let cancel propagate.
    for t in app_tasks:
        try:
            await t
        except asyncio.CancelledError:
            pass

_el2.run_until_complete(_run())
_el2.close()
",
                None,
                None,
            )
            .expect("run test");

            let send_errors: Vec<String> = py
                .eval(c"_mock2.errors", None, None)
                .expect("get send_errors")
                .extract()
                .expect("extract");
            assert!(
                send_errors.is_empty(),
                "CancelledError must not be forwarded to send_error: {send_errors:?}"
            );
        });
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
