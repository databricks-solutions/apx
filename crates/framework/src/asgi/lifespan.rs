//! ASGI lifespan protocol — startup/shutdown hooks for the application.
//!
//! Implements the ASGI lifespan spec: the server calls `app(scope, receive, send)`
//! with `scope["type"] == "lifespan"`, then exchanges startup/shutdown events via
//! the receive and send callables.
//!
//! The protocol runs on the asyncio thread as a long-lived task. Three tokio
//! oneshot channels bridge it to the tokio thread:
//! - **startup**: `LifespanSend` signals startup result
//! - **shutdown_trigger**: tokio thread tells `LifespanReceive` to deliver shutdown
//! - **shutdown**: `LifespanSend` signals shutdown result

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

use super::scope::{ResolvedAwaitable, ResolvedAwaitableWithValue};
use crate::io::EventLoop;

// ── Protocol types (pure, no I/O) ────────────────────────────────────────

/// Outcome of a lifespan startup or shutdown phase.
#[derive(Debug)]
pub enum LifespanResult {
    /// App sent `lifespan.startup.complete` or `lifespan.shutdown.complete`.
    Complete,
    /// App sent `lifespan.startup.failed` or `lifespan.shutdown.failed`.
    Failed(String),
    /// App raised during `app(scope, receive, send)` — does not support lifespan.
    Unsupported,
}

/// Internal state machine for [`LifespanReceive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveState {
    /// Next call returns `{"type": "lifespan.startup"}`.
    Startup,
    /// Next call blocks until shutdown trigger, then returns `{"type": "lifespan.shutdown"}`.
    WaitingShutdown,
    /// No more events — pend forever.
    Done,
}

// ── LifespanReceive ──────────────────────────────────────────────────────

/// ASGI `receive` callable for the lifespan protocol.
///
/// First `await receive()` returns `{"type": "lifespan.startup"}` immediately.
/// Second `await receive()` blocks until the server triggers shutdown, then
/// returns `{"type": "lifespan.shutdown"}`. Subsequent calls pend forever.
#[pyclass(module = "apx._core")]
pub struct LifespanReceive {
    state: Mutex<ReceiveState>,
    shutdown_trigger_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

crate::opaque_debug!(LifespanReceive);

impl LifespanReceive {
    /// Create a new lifespan receive callable.
    pub(crate) fn new(shutdown_trigger_rx: oneshot::Receiver<()>) -> Self {
        Self {
            state: Mutex::new(ReceiveState::Startup),
            shutdown_trigger_rx: Mutex::new(Some(shutdown_trigger_rx)),
        }
    }
}

#[pymethods]
impl LifespanReceive {
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("receive mutex poisoned"))?;

        match *state {
            ReceiveState::Startup => {
                *state = ReceiveState::WaitingShutdown;
                drop(state);
                let event = build_startup_event(py)?;
                Py::new(py, ResolvedAwaitableWithValue::new(event))
                    .map(|obj| obj.into_bound(py).into_any())
            }
            ReceiveState::WaitingShutdown => {
                let rx = self
                    .shutdown_trigger_rx
                    .lock()
                    .map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("shutdown trigger mutex poisoned")
                    })?
                    .take()
                    .ok_or_else(|| {
                        pyo3::exceptions::PyRuntimeError::new_err("shutdown already triggered")
                    })?;
                *state = ReceiveState::Done;
                drop(state);

                let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for lifespan shutdown wait",
                    )
                })?;
                let _guard = handle.enter();
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    let _ = rx.await;
                    Python::attach(|py| {
                        let event = build_shutdown_event(py)?;
                        Ok(event)
                    })
                })
            }
            ReceiveState::Done => {
                drop(state);
                let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for lifespan pending",
                    )
                })?;
                let _guard = handle.enter();
                pyo3_async_runtimes::tokio::future_into_py(
                    py,
                    std::future::pending::<PyResult<Py<PyAny>>>(),
                )
            }
        }
    }
}

// ── Send event classification (sans-I/O) ─────────────────────────────────

/// ASGI lifespan send event type: startup completed successfully.
const STARTUP_COMPLETE: &str = "lifespan.startup.complete";

/// ASGI lifespan send event type: startup failed.
const STARTUP_FAILED: &str = "lifespan.startup.failed";

/// ASGI lifespan send event type: shutdown completed successfully.
const SHUTDOWN_COMPLETE: &str = "lifespan.shutdown.complete";

/// ASGI lifespan send event type: shutdown failed.
const SHUTDOWN_FAILED: &str = "lifespan.shutdown.failed";

/// Classified lifespan send event — pure protocol, no I/O.
enum SendEvent {
    StartupComplete,
    StartupFailed(String),
    ShutdownComplete,
    ShutdownFailed(String),
}

/// Parse a lifespan send event dict into a classified event.
fn classify_send_event(event: &Bound<'_, PyDict>) -> PyResult<SendEvent> {
    let py = event.py();
    let event_type: String = event
        .get_item(pyo3::intern!(py, "type"))?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?
        .extract()?;

    match event_type.as_str() {
        STARTUP_COMPLETE => Ok(SendEvent::StartupComplete),
        STARTUP_FAILED => Ok(SendEvent::StartupFailed(extract_message(event)?)),
        SHUTDOWN_COMPLETE => Ok(SendEvent::ShutdownComplete),
        SHUTDOWN_FAILED => Ok(SendEvent::ShutdownFailed(extract_message(event)?)),
        _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unsupported lifespan event type: {event_type}"
        ))),
    }
}

/// Extract the optional `"message"` field from a lifespan event dict.
fn extract_message(event: &Bound<'_, PyDict>) -> PyResult<String> {
    let py = event.py();
    event
        .get_item(pyo3::intern!(py, "message"))?
        .map(|v| v.extract::<String>())
        .transpose()
        .map(|opt| opt.unwrap_or_default())
}

/// Send a result through a guarded oneshot channel.
fn signal(tx: &Mutex<Option<oneshot::Sender<LifespanResult>>>, result: LifespanResult) {
    if let Ok(mut guard) = tx.lock()
        && let Some(tx) = guard.take()
    {
        let _ = tx.send(result);
    }
}

// ── LifespanSend ─────────────────────────────────────────────────────────

/// ASGI `send` callable for the lifespan protocol.
///
/// Parses `lifespan.startup.complete`, `lifespan.startup.failed`,
/// `lifespan.shutdown.complete`, and `lifespan.shutdown.failed` events,
/// signaling results through oneshot channels.
#[pyclass(module = "apx._core")]
pub struct LifespanSend {
    startup_tx: Mutex<Option<oneshot::Sender<LifespanResult>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<LifespanResult>>>,
    resolved: Py<ResolvedAwaitable>,
}

crate::opaque_debug!(LifespanSend);

impl LifespanSend {
    /// Create a new lifespan send callable.
    pub(crate) fn new(
        py: Python<'_>,
        startup_tx: oneshot::Sender<LifespanResult>,
        shutdown_tx: oneshot::Sender<LifespanResult>,
    ) -> PyResult<Self> {
        Ok(Self {
            startup_tx: Mutex::new(Some(startup_tx)),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            resolved: Py::new(py, ResolvedAwaitable)?,
        })
    }
}

#[pymethods]
impl LifespanSend {
    /// Forward an unhandled app exception — signals lifespan unsupported or shutdown failed.
    fn send_error(&self, traceback: String) {
        if let Ok(mut guard) = self.startup_tx.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(LifespanResult::Unsupported);
            return;
        }
        signal(&self.shutdown_tx, LifespanResult::Failed(traceback));
    }

    fn __call__<'py>(
        &self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match classify_send_event(&event)? {
            SendEvent::StartupComplete => {
                signal(&self.startup_tx, LifespanResult::Complete);
            }
            SendEvent::StartupFailed(msg) => {
                signal(&self.startup_tx, LifespanResult::Failed(msg));
            }
            SendEvent::ShutdownComplete => {
                signal(&self.shutdown_tx, LifespanResult::Complete);
            }
            SendEvent::ShutdownFailed(msg) => {
                signal(&self.shutdown_tx, LifespanResult::Failed(msg));
            }
        }
        Ok(self.resolved.clone_ref(py).into_bound(py).into_any())
    }
}

// ── Scope builder ────────────────────────────────────────────────────────

/// ASGI protocol version string.
const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
const ASGI_SPEC_VERSION: &str = "2.4";

/// Build the ASGI lifespan scope dict.
fn build_lifespan_scope(py: Python<'_>) -> PyResult<Py<PyDict>> {
    let scope = PyDict::new(py);
    scope.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "lifespan"))?;

    let asgi = PyDict::new(py);
    asgi.set_item(
        pyo3::intern!(py, "version"),
        PyString::intern(py, ASGI_VERSION),
    )?;
    asgi.set_item(
        pyo3::intern!(py, "spec_version"),
        PyString::intern(py, ASGI_SPEC_VERSION),
    )?;
    scope.set_item(pyo3::intern!(py, "asgi"), asgi)?;

    scope.set_item(pyo3::intern!(py, "state"), PyDict::new(py))?;
    Ok(scope.unbind())
}

/// Build `{"type": "lifespan.startup"}` event for receive.
fn build_startup_event(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let event = PyDict::new(py);
    event.set_item(
        pyo3::intern!(py, "type"),
        pyo3::intern!(py, "lifespan.startup"),
    )?;
    Ok(event.into_any().unbind())
}

/// Build `{"type": "lifespan.shutdown"}` event for receive.
fn build_shutdown_event(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let event = PyDict::new(py);
    event.set_item(
        pyo3::intern!(py, "type"),
        pyo3::intern!(py, "lifespan.shutdown"),
    )?;
    Ok(event.into_any().unbind())
}

// ── Handles ──────────────────────────────────────────────────────────────

/// Pre-startup handle — awaiting startup result.
///
/// Returned by [`launch_lifespan`]. Call [`wait_startup`](Self::wait_startup)
/// to consume the startup channel and obtain a [`LifespanHandle`] for shutdown.
pub struct LifespanPending {
    startup_rx: oneshot::Receiver<LifespanResult>,
    shutdown_trigger_tx: oneshot::Sender<()>,
    shutdown_rx: oneshot::Receiver<LifespanResult>,
}

crate::opaque_debug!(LifespanPending);

/// Lifespan startup timeout — if the app does not respond within this
/// duration, startup is treated as a failure.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

impl LifespanPending {
    /// Wait for the app to complete lifespan startup.
    ///
    /// Returns `Ok(Some(handle))` on success, `Ok(None)` if the app does
    /// not support lifespan, or `Err(message)` on failure or timeout.
    pub async fn wait_startup(self) -> Result<Option<LifespanHandle>, String> {
        let result = tokio::time::timeout(STARTUP_TIMEOUT, self.startup_rx).await;

        match result {
            Ok(Ok(LifespanResult::Complete)) => Ok(Some(LifespanHandle {
                shutdown_trigger_tx: Some(self.shutdown_trigger_tx),
                shutdown_rx: Some(self.shutdown_rx),
            })),
            Ok(Ok(LifespanResult::Unsupported)) => Ok(None),
            Ok(Ok(LifespanResult::Failed(msg))) => Err(msg),
            Ok(Err(_)) => Err("lifespan task died unexpectedly".to_owned()),
            Err(_) => Err("lifespan startup timed out (30s)".to_owned()),
        }
    }
}

/// Post-startup handle — the lifespan coroutine is alive and waiting for shutdown.
pub struct LifespanHandle {
    shutdown_trigger_tx: Option<oneshot::Sender<()>>,
    shutdown_rx: Option<oneshot::Receiver<LifespanResult>>,
}

crate::opaque_debug!(LifespanHandle);

/// Lifespan shutdown timeout.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

impl LifespanHandle {
    /// Trigger lifespan shutdown and wait for completion.
    pub async fn trigger_shutdown(mut self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_trigger_tx.take() {
            let _ = tx.send(());
        }
        let Some(rx) = self.shutdown_rx.take() else {
            return Ok(());
        };
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, rx).await {
            Ok(Ok(LifespanResult::Failed(msg))) => Err(msg),
            Ok(Ok(LifespanResult::Complete | LifespanResult::Unsupported) | Err(_)) => Ok(()),
            Err(_) => {
                tracing::warn!(
                    name: "apx.lifespan.shutdown_timeout",
                    "lifespan shutdown timed out (30s)"
                );
                Ok(())
            }
        }
    }
}

// ── Launcher ─────────────────────────────────────────────────────────────

/// Launch the ASGI lifespan protocol on the asyncio thread.
///
/// Builds the lifespan scope, receive, and send callables, then submits
/// `launch(app, scope, receive, send)` via `call_soon_threadsafe`.
/// Returns a [`LifespanPending`] for awaiting the startup result.
pub fn launch_lifespan(
    py: Python<'_>,
    event_loop: &EventLoop,
    app: &Py<PyAny>,
    launch_fn: &Py<PyAny>,
) -> PyResult<LifespanPending> {
    let (startup_tx, startup_rx) = oneshot::channel();
    let (shutdown_trigger_tx, shutdown_trigger_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let scope = build_lifespan_scope(py)?;
    let receive = Py::new(py, LifespanReceive::new(shutdown_trigger_rx))?;
    let send = Py::new(py, LifespanSend::new(py, startup_tx, shutdown_tx)?)?;

    event_loop
        .call_soon_threadsafe()
        .call1(py, (launch_fn, app, &scope, &receive, &send))?;

    Ok(LifespanPending {
        startup_rx,
        shutdown_trigger_tx,
        shutdown_rx,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::with_py;

    #[test]
    fn build_lifespan_scope_fields() {
        with_py(|py| {
            let scope = build_lifespan_scope(py).unwrap();
            let scope = scope.bind(py);

            let scope_type: String = scope.get_item("type").unwrap().unwrap().extract().unwrap();
            assert_eq!(scope_type, "lifespan");

            let asgi = scope.get_item("asgi").unwrap().unwrap();
            let version: String = asgi.get_item("version").unwrap().extract().unwrap();
            assert_eq!(version, "3.0");
            let spec: String = asgi.get_item("spec_version").unwrap().extract().unwrap();
            assert_eq!(spec, "2.4");

            let state = scope.get_item("state").unwrap().unwrap();
            assert_eq!(state.len().unwrap(), 0);
        });
    }

    #[test]
    fn startup_event_has_correct_type() {
        with_py(|py| {
            let event = build_startup_event(py).unwrap();
            let event = event.bind(py);
            let t: String = event.get_item("type").unwrap().extract().unwrap();
            assert_eq!(t, "lifespan.startup");
        });
    }

    #[test]
    fn shutdown_event_has_correct_type() {
        with_py(|py| {
            let event = build_shutdown_event(py).unwrap();
            let event = event.bind(py);
            let t: String = event.get_item("type").unwrap().extract().unwrap();
            assert_eq!(t, "lifespan.shutdown");
        });
    }

    #[test]
    fn lifespan_send_startup_complete() {
        with_py(|py| {
            let (startup_tx, mut startup_rx) = oneshot::channel();
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.complete").unwrap();
            send.__call__(py, event).unwrap();

            let result = startup_rx.try_recv().unwrap();
            assert!(matches!(result, LifespanResult::Complete));
        });
    }

    #[test]
    fn lifespan_send_startup_failed() {
        with_py(|py| {
            let (startup_tx, mut startup_rx) = oneshot::channel();
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.failed").unwrap();
            event.set_item("message", "db connection refused").unwrap();
            send.__call__(py, event).unwrap();

            let result = startup_rx.try_recv().unwrap();
            assert!(
                matches!(&result, LifespanResult::Failed(msg) if msg == "db connection refused"),
                "expected Failed(\"db connection refused\"), got {result:?}"
            );
        });
    }

    #[test]
    fn lifespan_send_startup_failed_no_message() {
        with_py(|py| {
            let (startup_tx, mut startup_rx) = oneshot::channel();
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.failed").unwrap();
            send.__call__(py, event).unwrap();

            let result = startup_rx.try_recv().unwrap();
            assert!(
                matches!(&result, LifespanResult::Failed(msg) if msg.is_empty()),
                "expected Failed(\"\"), got {result:?}"
            );
        });
    }

    #[test]
    fn lifespan_send_shutdown_complete() {
        with_py(|py| {
            let (startup_tx, _startup_rx) = oneshot::channel();
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            // Consume startup first (simulates normal flow).
            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.complete").unwrap();
            send.__call__(py, event).unwrap();

            let event = PyDict::new(py);
            event
                .set_item("type", "lifespan.shutdown.complete")
                .unwrap();
            send.__call__(py, event).unwrap();

            let result = shutdown_rx.try_recv().unwrap();
            assert!(matches!(result, LifespanResult::Complete));
        });
    }

    #[test]
    fn lifespan_send_shutdown_failed() {
        with_py(|py| {
            let (startup_tx, _startup_rx) = oneshot::channel();
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            // Consume startup first.
            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.complete").unwrap();
            send.__call__(py, event).unwrap();

            let event = PyDict::new(py);
            event.set_item("type", "lifespan.shutdown.failed").unwrap();
            event.set_item("message", "cleanup error").unwrap();
            send.__call__(py, event).unwrap();

            let result = shutdown_rx.try_recv().unwrap();
            assert!(
                matches!(&result, LifespanResult::Failed(msg) if msg == "cleanup error"),
                "expected Failed(\"cleanup error\"), got {result:?}"
            );
        });
    }

    #[test]
    fn lifespan_send_unknown_event_type() {
        with_py(|py| {
            let (startup_tx, _startup_rx) = oneshot::channel();
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            let event = PyDict::new(py);
            event.set_item("type", "lifespan.unknown").unwrap();
            let result = send.__call__(py, event);
            assert!(result.is_err());
        });
    }

    #[test]
    fn send_error_during_startup_signals_unsupported() {
        with_py(|py| {
            let (startup_tx, mut startup_rx) = oneshot::channel();
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            send.send_error("TypeError: ...".to_owned());

            let result = startup_rx.try_recv().unwrap();
            assert!(matches!(result, LifespanResult::Unsupported));
        });
    }

    #[test]
    fn send_error_during_shutdown_signals_failed() {
        with_py(|py| {
            let (startup_tx, _startup_rx) = oneshot::channel();
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let send = LifespanSend::new(py, startup_tx, shutdown_tx).unwrap();

            // Consume startup to transition phase.
            let event = PyDict::new(py);
            event.set_item("type", "lifespan.startup.complete").unwrap();
            send.__call__(py, event).unwrap();

            send.send_error("RuntimeError: cleanup failed".to_owned());

            let result = shutdown_rx.try_recv().unwrap();
            assert!(
                matches!(&result, LifespanResult::Failed(msg) if msg.contains("cleanup failed")),
                "expected Failed containing \"cleanup failed\", got {result:?}"
            );
        });
    }

    #[test]
    fn lifespan_receive_first_call_returns_startup() {
        with_py(|py| {
            let (_tx, rx) = oneshot::channel();
            let receive = LifespanReceive::new(rx);

            let awaitable = receive.__call__(py).unwrap();
            // The awaitable should be a ResolvedAwaitableWithValue.
            // We can check it implements __await__.
            assert!(awaitable.hasattr("__await__").unwrap());
        });
    }

    #[test]
    fn lifespan_result_debug() {
        let c = LifespanResult::Complete;
        assert!(format!("{c:?}").contains("Complete"));
        let f = LifespanResult::Failed("err".to_owned());
        assert!(format!("{f:?}").contains("Failed"));
        let u = LifespanResult::Unsupported;
        assert!(format!("{u:?}").contains("Unsupported"));
    }

    #[tokio::test]
    async fn lifespan_pending_wait_startup_complete() {
        let (startup_tx, startup_rx) = oneshot::channel();
        let (shutdown_trigger_tx, _shutdown_trigger_rx) = oneshot::channel();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        let pending = LifespanPending {
            startup_rx,
            shutdown_trigger_tx,
            shutdown_rx,
        };

        let _ = startup_tx.send(LifespanResult::Complete);
        let result = pending.wait_startup().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[tokio::test]
    async fn lifespan_pending_wait_startup_unsupported() {
        let (startup_tx, startup_rx) = oneshot::channel();
        let (shutdown_trigger_tx, _shutdown_trigger_rx) = oneshot::channel();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        let pending = LifespanPending {
            startup_rx,
            shutdown_trigger_tx,
            shutdown_rx,
        };

        let _ = startup_tx.send(LifespanResult::Unsupported);
        let result = pending.wait_startup().await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn lifespan_pending_wait_startup_failed() {
        let (startup_tx, startup_rx) = oneshot::channel();
        let (shutdown_trigger_tx, _shutdown_trigger_rx) = oneshot::channel();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        let pending = LifespanPending {
            startup_rx,
            shutdown_trigger_tx,
            shutdown_rx,
        };

        let _ = startup_tx.send(LifespanResult::Failed("db error".to_owned()));
        let result = pending.wait_startup().await;
        assert_eq!(result.unwrap_err(), "db error");
    }

    #[tokio::test]
    async fn lifespan_handle_trigger_shutdown_complete() {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (trigger_tx, trigger_rx) = oneshot::channel();

        let handle = LifespanHandle {
            shutdown_trigger_tx: Some(trigger_tx),
            shutdown_rx: Some(shutdown_rx),
        };

        // Simulate the app responding to shutdown trigger.
        tokio::spawn(async move {
            let _ = trigger_rx.await;
            let _ = shutdown_tx.send(LifespanResult::Complete);
        });

        let result = handle.trigger_shutdown().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn lifespan_handle_trigger_shutdown_failed() {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (trigger_tx, trigger_rx) = oneshot::channel();

        let handle = LifespanHandle {
            shutdown_trigger_tx: Some(trigger_tx),
            shutdown_rx: Some(shutdown_rx),
        };

        tokio::spawn(async move {
            let _ = trigger_rx.await;
            let _ = shutdown_tx.send(LifespanResult::Failed("cleanup err".to_owned()));
        });

        let result = handle.trigger_shutdown().await;
        assert_eq!(result.unwrap_err(), "cleanup err");
    }
}
