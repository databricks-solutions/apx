//! Single worker: initialize Python, bind TCP, serve requests.
//!
//! A worker is a child process spawned by the supervisor. It owns one Python
//! interpreter, one inline asyncio event loop, and one TCP listener bound via
//! `SO_REUSEPORT`.

use super::ipc::channel::WorkerChannel;
use super::ipc::protocol::{BootstrapError, IpcMessage, Nonce, WorkerBootstrap};
use super::signal::shutdown_signal;
use super::worker_context::WorkerContext;
use crate::asgi::app::{AppSource, ModuleImport};
use crate::io::EventLoop;
use crate::protocol::http::service::{ApxService, ServiceConfig, serve_tcp};
use crate::transport::{Listener, TransportConfig, TransportError};
use pyo3::prelude::*;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// Minimum drain timeout (seconds) even if request_timeout_secs is lower.
const MIN_DRAIN_TIMEOUT_SECS: u64 = 5;

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

/// Phase 1 runtime: TCP listener + Python interpreter (expensive, survives reloads).
pub struct WorkerRuntime {
    /// TCP listener bound via the `Listener` trait.
    pub listener: crate::transport::TcpListener,
    /// IPC channel to supervisor — stays open for the worker's lifetime.
    pub channel: WorkerChannel,
    /// Asyncio event loop (dedicated thread, asyncio delegation).
    pub event_loop: EventLoop,
}

impl std::fmt::Debug for WorkerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerRuntime").finish_non_exhaustive()
    }
}

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
    let ctx = {
        let el = &runtime.event_loop;
        Python::attach(|py| -> Result<Arc<WorkerContext>, WorkerError> {
            let guarded_fn = register_guarded(py)
                .map_err(|e| WorkerError::PythonInit(format!("register _guarded: {e}")))?;
            Ok(Arc::new(WorkerContext {
                call_soon_threadsafe: el.call_soon_threadsafe().clone_ref(py),
                create_task: el.create_task().clone_ref(py),
                guarded_fn,
            }))
        })?
    };

    // Load app and build dispatch pipeline.
    let server_addr = runtime.listener.local_addr();
    let dispatch = Python::attach(|py| {
        ModuleImport::new(bootstrap.app_module.as_str()).build(py, ctx, server_addr)
    })?;

    // Read telemetry config from Python (after app load, so user configure() ran).
    let telemetry_config = Python::attach(|py| {
        crate::telemetry::bootstrap_python_telemetry(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry bootstrap: {e}")))?;
        crate::telemetry::config::read_python_config(py)
            .map_err(|e| WorkerError::PythonInit(format!("telemetry config: {e}")))
    })?;

    let _system_metrics_handle = if telemetry_config.system.enabled {
        Some(crate::telemetry::system_metrics::spawn_system_metrics(
            &telemetry_config.system,
        ))
    } else {
        None
    };

    // Build HTTP service.
    let mut config = ServiceConfig {
        timeout: Duration::from_secs(bootstrap.request_timeout_secs),
        ..ServiceConfig::default()
    };
    if let Some(mc) = bootstrap.max_concurrent {
        config.max_concurrent = mc;
    }
    let server_addr = runtime.listener.local_addr();
    let service = ApxService::new(dispatch, server_addr, &config);

    // Split IPC channel for concurrent read/write.
    let (ipc_reader, mut ipc_writer) = runtime.channel.split();

    // Spawn drain listener — waits for supervisor's Drain command.
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut reader = ipc_reader;
        match reader.recv().await {
            Ok(IpcMessage::Drain) => {
                tracing::info!("received Drain from supervisor");
                let _ = drain_tx.send(());
            }
            Ok(msg) => tracing::warn!(?msg, "unexpected IPC message"),
            Err(e) => tracing::debug!(error = %e, "IPC channel closed"),
        }
    });

    // Combined shutdown: OS signal OR IPC drain.
    let combined_shutdown = async {
        tokio::select! {
            () = shutdown_signal() => {}
            _ = drain_rx => {}
        }
    };

    let mut connections = serve_tcp(runtime.listener, service, combined_shutdown)
        .await
        .map_err(WorkerError::Serve)?;

    // Drain in-flight connections (bounded by request timeout).
    let drain_timeout =
        Duration::from_secs(bootstrap.request_timeout_secs.max(MIN_DRAIN_TIMEOUT_SECS));
    let _ = tokio::time::timeout(drain_timeout, async {
        while connections.join_next().await.is_some() {}
    })
    .await;

    // Best-effort: tell supervisor we're done draining.
    let _ = ipc_writer.send(&IpcMessage::Drained).await;

    // Flush pending OTLP spans, metrics, and logs before the event loop stops.
    apx_core::tracing_init::shutdown_telemetry();
    runtime.event_loop.shutdown();

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

// ── _guarded wrapper ────────────────────────────────────────────────────

/// Python source for the `_guarded` error-forwarding wrapper.
///
/// Wraps an ASGI coroutine so that application exceptions (`Exception`)
/// are forwarded through `AsgiSend.send_error()` as 500 responses.
/// The exception is **not** re-raised: the task is fire-and-forget
/// (`create_task` with no `await`), so re-raising would cause asyncio
/// to log "Task exception was never retrieved" on every app error.
///
/// `CancelledError` and other `BaseException` subclasses propagate
/// naturally — they are control flow signals, not app errors.
const GUARDED_SOURCE: &str = r#"
import traceback as _tb

async def _guarded(coro, send):
    try:
        await coro
    except Exception as exc:
        tb = "".join(_tb.format_exception(type(exc), exc, exc.__traceback__))
        send.send_error(tb)
"#;

/// Register the `_guarded` Python function and return a reference to it.
fn register_guarded(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let globals = pyo3::types::PyDict::new(py);
    let source = std::ffi::CString::new(GUARDED_SOURCE).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid guarded source: {e}"))
    })?;
    py.run(&source, Some(&globals), None)?;
    globals
        .get_item("_guarded")?
        .ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("_guarded function not found after exec")
        })
        .map(|f| f.unbind())
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

    /// `_guarded` must forward app exceptions through `send.send_error()`
    /// without re-raising — otherwise asyncio logs "Task exception was
    /// never retrieved" on every app error (the task is fire-and-forget).
    #[test]
    fn guarded_forwards_error_without_asyncio_leak() {
        crate::with_py(|py| {
            py.run(
                c"
import asyncio, gc, builtins

_leak_errors = []

def _capture(loop, ctx):
    _leak_errors.append(ctx.get('message', ''))

class _MockSend:
    def __init__(self):
        self.errors = []
    def send_error(self, tb):
        self.errors.append(tb)

_mock = _MockSend()

async def _fail():
    raise RuntimeError('deliberate test error')
",
                None,
                None,
            )
            .expect("define fixtures");

            let guarded_fn = register_guarded(py).expect("register_guarded");

            let mock = py.eval(c"_mock", None, None).expect("get mock");
            let coro = py.eval(c"_fail()", None, None).expect("create coro");
            let guarded_coro = guarded_fn
                .call1(py, (&coro, &mock))
                .expect("wrap in _guarded");

            py.import(c"builtins")
                .expect("import builtins")
                .setattr(c"_test_gcoro", &guarded_coro)
                .expect("store guarded coro");

            py.run(
                c"
_el = asyncio.new_event_loop()
_el.set_exception_handler(_capture)

async def _run():
    _el.create_task(builtins._test_gcoro)
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    gc.collect()
    gc.collect()
    await asyncio.sleep(0)

_el.run_until_complete(_run())
_el.close()
",
                None,
                None,
            )
            .expect("run test");

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
                "_guarded re-raised, causing asyncio log spam: {task_leaks:?}"
            );
        });
    }

    /// `CancelledError` must propagate through `_guarded` — it's a control
    /// flow signal, not an app error. It must NOT be forwarded to
    /// `send.send_error()`.
    #[test]
    fn guarded_propagates_cancellation() {
        crate::with_py(|py| {
            py.run(
                c"
import asyncio, builtins

class _MockSend:
    def __init__(self):
        self.errors = []
    def send_error(self, tb):
        self.errors.append(tb)

_mock2 = _MockSend()

async def _slow():
    await asyncio.sleep(10)
",
                None,
                None,
            )
            .expect("define fixtures");

            let guarded_fn = register_guarded(py).expect("register_guarded");

            let mock = py.eval(c"_mock2", None, None).expect("get mock");
            let coro = py.eval(c"_slow()", None, None).expect("create coro");
            let guarded_coro = guarded_fn
                .call1(py, (&coro, &mock))
                .expect("wrap in _guarded");

            py.import(c"builtins")
                .expect("import builtins")
                .setattr(c"_test_gcoro2", &guarded_coro)
                .expect("store guarded coro");

            py.run(
                c"
_el2 = asyncio.new_event_loop()

async def _run():
    task = _el2.create_task(builtins._test_gcoro2)
    await asyncio.sleep(0)
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        pass
    return task.cancelled()

_cancelled = _el2.run_until_complete(_run())
_el2.close()
",
                None,
                None,
            )
            .expect("run test");

            let cancelled: bool = py
                .eval(c"_cancelled", None, None)
                .expect("get result")
                .extract()
                .expect("extract");
            assert!(cancelled, "task must be properly cancelled");

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
}
