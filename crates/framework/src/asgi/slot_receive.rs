//! ASGI `receive()` callable for the 3-thread dispatch pipeline.
//!
//! [`SlotReceive`] wraps a pre-collected request body from [`RequestSlot`].
//! First call returns `http.request` with the body as a resolved awaitable.
//! Subsequent calls pend indefinitely (disconnect watch).

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use super::scope::ResolvedAwaitableWithValue;

/// ASGI `receive` callable for the 3-thread pipeline.
///
/// Runs entirely on Thread 2 (Python thread, 100% GIL). The body is
/// pre-collected on Thread 1 and passed in as `Bytes`.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct SlotReceive {
    body: std::sync::Mutex<Option<Bytes>>,
    receive_template: Py<PyDict>,
}

crate::opaque_debug!(SlotReceive);

impl SlotReceive {
    /// Create for an HTTP request with a pre-collected body.
    pub fn new(body: Bytes, receive_template: Py<PyDict>) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(body)),
            receive_template,
        }
    }
}

#[pymethods]
impl SlotReceive {
    /// `event = await receive()`
    ///
    /// First call: returns `http.request` with body via `ResolvedAwaitableWithValue`.
    /// Subsequent calls: pend forever (disconnect watch — the connection
    /// outlives the ASGI handler in normal operation).
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let taken = self
            .body
            .lock()
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("receive mutex poisoned"))?
            .take();

        if let Some(bytes) = taken {
            let event = crate::telemetry::timed!(
                crate::telemetry::dispatch_metrics::record_receive_build,
                {
                    let event = self.receive_template.bind(py).copy()?;
                    event.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, &bytes))?;
                    event
                }
            );
            let event = event.unbind().into_any();
            Py::new(py, ResolvedAwaitableWithValue::new(event))
                .map(|obj| obj.into_bound(py).into_any())
        } else {
            let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("no tokio runtime for disconnect watch")
            })?;
            let _guard = handle.enter();
            pyo3_async_runtimes::tokio::future_into_py(
                py,
                std::future::pending::<PyResult<Py<PyAny>>>(),
            )
        }
    }
}
