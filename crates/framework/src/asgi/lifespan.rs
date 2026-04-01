//! ASGI lifespan protocol — startup/shutdown hooks for the application.
//!
//! Implements the ASGI lifespan spec: the server calls
//! `app(scope, receive, send)` with `scope["type"] == "lifespan"`,
//! then exchanges startup/shutdown events via the receive and send callables.
//!
//! Lifespan runs as an asyncio task on the event loop. Signaling uses
//! `asyncio.Event` for startup/shutdown coordination.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Mutex;

use super::scope::{ResolvedAwaitable, ResolvedAwaitableWithValue};

/// Internal state machine for [`LifespanReceive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveState {
    /// Next call returns `{"type": "lifespan.startup"}`.
    Startup,
    /// Next call waits for shutdown, then returns `{"type": "lifespan.shutdown"}`.
    WaitingShutdown,
    /// No more events — pend forever.
    Done,
}

// ── LifespanReceive ──────────────────────────────────────────────────────

/// ASGI `receive` callable for the lifespan protocol.
///
/// First `await receive()` returns `{"type": "lifespan.startup"}` immediately.
/// Second `await receive()` waits for the shutdown event, then returns
/// `{"type": "lifespan.shutdown"}`.
#[pyclass(module = "apx._core")]
pub struct LifespanReceive {
    state: Mutex<ReceiveState>,
    shutdown_event: Py<PyAny>,
}

crate::opaque_debug!(LifespanReceive);

#[pymethods]
impl LifespanReceive {
    #[new]
    fn new(shutdown_event: Py<PyAny>) -> Self {
        Self {
            state: Mutex::new(ReceiveState::Startup),
            shutdown_event,
        }
    }

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
                *state = ReceiveState::Done;
                drop(state);
                build_shutdown_awaitable(py, &self.shutdown_event)
            }
            ReceiveState::Done => {
                drop(state);
                let fut = py
                    .import(c"asyncio")?
                    .call_method0(pyo3::intern!(py, "get_running_loop"))?
                    .call_method0(pyo3::intern!(py, "create_future"))?;
                Ok(fut)
            }
        }
    }
}

/// Build an awaitable that waits for the shutdown event, then returns the
/// lifespan.shutdown event dict.
fn build_shutdown_awaitable<'py>(
    py: Python<'py>,
    shutdown_event: &Py<PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let globals = PyDict::new(py);
    globals.set_item("_shutdown_event", shutdown_event.bind(py))?;
    let locals = PyDict::new(py);

    py.run(
        c"
import asyncio

async def _wait_shutdown():
    await _shutdown_event.wait()
    return {'type': 'lifespan.shutdown'}

_coro = _wait_shutdown()
",
        Some(&globals),
        Some(&locals),
    )?;

    let coro = locals
        .get_item(pyo3::intern!(py, "_coro"))?
        .ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("failed to create shutdown coro")
        })?;
    Ok(coro.clone())
}

// ── Send event classification ────────────────────────────────────────────

/// ASGI lifespan send event types.
const STARTUP_COMPLETE: &str = "lifespan.startup.complete";
/// ASGI lifespan startup failed event.
const STARTUP_FAILED: &str = "lifespan.startup.failed";
/// ASGI lifespan shutdown complete event.
const SHUTDOWN_COMPLETE: &str = "lifespan.shutdown.complete";
/// ASGI lifespan shutdown failed event.
const SHUTDOWN_FAILED: &str = "lifespan.shutdown.failed";

/// Classified lifespan send event.
enum SendEvent {
    /// Startup completed.
    StartupComplete,
    /// Startup failed with message.
    StartupFailed(String),
    /// Shutdown completed.
    ShutdownComplete,
    /// Shutdown failed with message.
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

// ── LifespanSend ─────────────────────────────────────────────────────────

/// ASGI `send` callable for the lifespan protocol.
///
/// Parses startup/shutdown events and signals results via Python
/// `asyncio.Event` objects and a shared result slot.
#[pyclass(module = "apx._core")]
pub struct LifespanSend {
    startup_result: Py<PyAny>,
    shutdown_result: Py<PyAny>,
    startup_event: Py<PyAny>,
    shutdown_done_event: Py<PyAny>,
    resolved: Py<ResolvedAwaitable>,
}

crate::opaque_debug!(LifespanSend);

#[pymethods]
impl LifespanSend {
    #[new]
    fn new(
        py: Python<'_>,
        startup_event: Py<PyAny>,
        startup_result: Py<PyAny>,
        shutdown_done_event: Py<PyAny>,
        shutdown_result: Py<PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            startup_result,
            shutdown_result,
            startup_event,
            shutdown_done_event,
            resolved: Py::new(py, ResolvedAwaitable)?,
        })
    }

    /// Forward an unhandled app exception — signals lifespan unsupported.
    fn send_error(&self, py: Python<'_>, _traceback: String) -> PyResult<()> {
        self.startup_result.call_method1(
            py,
            pyo3::intern!(py, "__setitem__"),
            (0, "unsupported"),
        )?;
        self.startup_event
            .call_method0(py, pyo3::intern!(py, "set"))?;
        Ok(())
    }

    fn __call__<'py>(
        &self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match classify_send_event(&event)? {
            SendEvent::StartupComplete => {
                self.startup_result.call_method1(
                    py,
                    pyo3::intern!(py, "__setitem__"),
                    (0, "complete"),
                )?;
                self.startup_event
                    .call_method0(py, pyo3::intern!(py, "set"))?;
            }
            SendEvent::StartupFailed(msg) => {
                let val = format!("failed:{msg}");
                self.startup_result
                    .call_method1(py, pyo3::intern!(py, "__setitem__"), (0, val))?;
                self.startup_event
                    .call_method0(py, pyo3::intern!(py, "set"))?;
            }
            SendEvent::ShutdownComplete => {
                self.shutdown_result.call_method1(
                    py,
                    pyo3::intern!(py, "__setitem__"),
                    (0, "complete"),
                )?;
                self.shutdown_done_event
                    .call_method0(py, pyo3::intern!(py, "set"))?;
            }
            SendEvent::ShutdownFailed(msg) => {
                let val = format!("failed:{msg}");
                self.shutdown_result.call_method1(
                    py,
                    pyo3::intern!(py, "__setitem__"),
                    (0, val),
                )?;
                self.shutdown_done_event
                    .call_method0(py, pyo3::intern!(py, "set"))?;
            }
        }
        Ok(self.resolved.clone_ref(py).into_bound(py).into_any())
    }
}

// ── Scope builder ────────────────────────────────────────────────────────

/// Build the ASGI lifespan scope dict.
#[cfg(test)]
pub fn build_lifespan_scope(py: Python<'_>) -> PyResult<Py<PyDict>> {
    use pyo3::types::PyString;

    use super::{ASGI_SPEC_VERSION, ASGI_VERSION};

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
    fn build_startup_event_type() {
        with_py(|py| {
            let event = build_startup_event(py).unwrap();
            let event = event.bind(py);
            let t: String = event.get_item("type").unwrap().extract().unwrap();
            assert_eq!(t, "lifespan.startup");
        });
    }
}
