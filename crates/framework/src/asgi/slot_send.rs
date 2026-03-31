//! ASGI `send()` callable for the 2-thread dispatch pipeline.
//!
//! [`SlotSend`] collects `http.response.start` (status + headers) and on
//! the first `http.response.body` creates an mpsc channel, builds a
//! [`ResponseData`], and fires the tokio oneshot directly to Thread 1.
//! Subsequent body chunks are pushed via the mpsc sender. Dropping the
//! sender signals EOF.

use crate::io::channel::ResponseData;
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::{PyBytes, PyDict};
use tokio::sync::{mpsc, oneshot};

use super::scope::ResolvedAwaitable;

/// Generic 500 body returned to clients in production mode.
const INTERNAL_ERROR_BODY: Bytes = Bytes::from_static(b"Internal Server Error");

// ── SlotSend ─────────────────────────────────────────────────────────────

/// ASGI `send` callable for the 2-thread pipeline.
///
/// Runs entirely on Thread 2 (Python thread, 100% GIL). On the first
/// body chunk, creates the response and fires the tokio oneshot directly
/// to Thread 1.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct SlotSend {
    status: Option<u16>,
    raw_headers: Option<Vec<(Bytes, Bytes)>>,
    response_tx: Option<oneshot::Sender<ResponseData>>,
    body_tx: Option<mpsc::UnboundedSender<Bytes>>,
    resolved: Py<ResolvedAwaitable>,
    dev_mode: bool,
}

impl std::fmt::Debug for SlotSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotSend")
            .field("status", &self.status)
            .field("has_response_tx", &self.response_tx.is_some())
            .field("streaming", &self.body_tx.is_some())
            .finish_non_exhaustive()
    }
}

impl SlotSend {
    /// Create a new `SlotSend` for an HTTP request.
    pub(crate) fn new(
        response_tx: oneshot::Sender<ResponseData>,
        resolved: Py<ResolvedAwaitable>,
        dev_mode: bool,
    ) -> Self {
        Self {
            status: None,
            raw_headers: None,
            response_tx: Some(response_tx),
            body_tx: None,
            resolved,
            dev_mode,
        }
    }
}

#[pymethods]
impl SlotSend {
    /// Forward an unhandled app exception as a 500 response.
    ///
    /// Called by the `_guarded` wrapper in `_dispatch.py` when the ASGI
    /// app raises an `Exception`. Always logs the full traceback
    /// server-side; the response body depends on `dev_mode`.
    fn send_error(&mut self, traceback: String) {
        tracing::error!(
            name: "apx.dispatch.unhandled_exception",
            "{traceback}",
        );
        if let Some(response_tx) = self.response_tx.take() {
            let (body_tx, body_rx) = mpsc::unbounded_channel();
            let body = if self.dev_mode {
                Bytes::from(traceback)
            } else {
                INTERNAL_ERROR_BODY
            };
            let _ = body_tx.send(body);
            drop(body_tx);
            let response = ResponseData {
                status: 500,
                headers: vec![(
                    Bytes::from_static(b"content-type"),
                    Bytes::from_static(b"text/plain; charset=utf-8"),
                )],
                body_rx,
            };
            let _ = response_tx.send(response);
        }
    }

    /// `await send({"type": "http.response.start"|"http.response.body", ...})`
    fn __call__<'py>(
        &mut self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let type_obj = event
            .get_item(pyo3::intern!(py, "type"))?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?;

        if type_obj.eq(pyo3::intern!(py, "http.response.start"))? {
            crate::telemetry::timed!(
                crate::telemetry::dispatch_metrics::record_send_parse,
                self.handle_response_start(py, &event)
            )
        } else if type_obj.eq(pyo3::intern!(py, "http.response.body"))? {
            let (body, more_body) =
                crate::telemetry::timed!(crate::telemetry::dispatch_metrics::record_send_parse, {
                    let body = extract_body_bytes(&event)?;
                    let more_body: bool = event
                        .get_item(pyo3::intern!(py, "more_body"))?
                        .map(|b| b.extract())
                        .transpose()?
                        .unwrap_or(false);
                    (body, more_body)
                });

            if self.body_tx.is_none() {
                self.send_first_body_chunk(body, more_body)?;
            } else {
                self.send_subsequent_chunk(body, more_body);
            }
            Ok(self.resolved.clone_ref(py).into_bound(py).into_any())
        } else {
            let event_type: String = type_obj.extract()?;
            Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unsupported ASGI event type: {event_type}"
            )))
        }
    }
}

impl SlotSend {
    /// Handle `http.response.start` — extract status + headers.
    fn handle_response_start<'py>(
        &mut self,
        py: Python<'py>,
        event: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let status: u16 = event
            .get_item(pyo3::intern!(py, "status"))?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("status"))?
            .extract()?;
        let headers = extract_raw_headers(event)?;
        self.status = Some(status);
        self.raw_headers = Some(headers);
        Ok(self.resolved.clone_ref(py).into_bound(py).into_any())
    }

    /// First body chunk: create mpsc, build `ResponseData`, push `OutboundSlot`.
    fn send_first_body_chunk(&mut self, body: Bytes, more_body: bool) -> PyResult<()> {
        let status = self.status.take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "ASGI protocol error: body before response start",
            )
        })?;
        let headers = self.raw_headers.take().unwrap_or_default();

        let (body_tx, body_rx) = mpsc::unbounded_channel();
        if !body.is_empty() {
            let _ = body_tx.send(body);
        }

        if more_body {
            self.body_tx = Some(body_tx);
        } else {
            drop(body_tx);
        }

        let response = ResponseData {
            status,
            headers,
            body_rx,
        };

        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(response);
        }

        Ok(())
    }

    /// Subsequent body chunks: push via mpsc, drop sender on EOF.
    fn send_subsequent_chunk(&mut self, body: Bytes, more_body: bool) {
        if let Some(tx) = &self.body_tx {
            let _ = tx.send(body);
        }
        if !more_body {
            self.body_tx = None;
        }
    }
}

// ── Header extraction ────────────────────────────────────────────────────

/// Extract response headers as raw byte pairs from the ASGI event dict.
///
/// Returns `(name, value)` pairs as `Bytes` for zero-copy transfer to
/// Thread 1. The ASGI spec represents headers as a list of 2-tuples
/// of byte strings.
fn extract_raw_headers(event: &Bound<'_, PyDict>) -> PyResult<Vec<(Bytes, Bytes)>> {
    let py = event.py();
    let Some(obj) = event.get_item(pyo3::intern!(py, "headers"))? else {
        return Ok(vec![]);
    };
    let iter = obj.try_iter()?;
    let mut result = Vec::with_capacity(8);
    for item in iter {
        let pair = item?;
        let tuple = pair.cast::<pyo3::types::PyTuple>()?;
        let name = extract_bytes_from_obj(&tuple.get_item(0)?)?;
        let value = extract_bytes_from_obj(&tuple.get_item(1)?)?;
        result.push((name, value));
    }
    Ok(result)
}

/// Extract `Bytes` from a Python bytes object via zero-copy `PyBackedBytes`.
fn extract_bytes_from_obj(obj: &Bound<'_, PyAny>) -> PyResult<Bytes> {
    match obj.cast::<PyBytes>() {
        Ok(py_bytes) => {
            let backed: PyBackedBytes = py_bytes.clone().into();
            Ok(Bytes::from_owner(backed))
        }
        Err(_) => Ok(Bytes::from(obj.extract::<Vec<u8>>()?)),
    }
}

/// Extract body bytes from an ASGI event dict.
fn extract_body_bytes(event: &Bound<'_, PyDict>) -> PyResult<Bytes> {
    let py = event.py();
    let Some(obj) = event.get_item(pyo3::intern!(py, "body"))? else {
        return Ok(Bytes::new());
    };
    match obj.cast::<PyBytes>() {
        Ok(py_bytes) => {
            let backed: PyBackedBytes = py_bytes.clone().into();
            Ok(Bytes::from_owner(backed))
        }
        Err(_) => Ok(Bytes::from(obj.extract::<Vec<u8>>()?)),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    fn make_slot_send(dev_mode: bool) -> (SlotSend, oneshot::Receiver<ResponseData>) {
        let (tx, rx) = oneshot::channel();
        let resolved = crate::with_py(|py| Py::new(py, ResolvedAwaitable).unwrap());
        (SlotSend::new(tx, resolved, dev_mode), rx)
    }

    #[test]
    fn send_error_prod_mode_returns_generic_body() {
        let (mut slot, mut rx) = make_slot_send(false);
        let traceback = "Traceback (most recent call last):\n  NameError: x\n".to_owned();
        slot.send_error(traceback);

        let mut response = rx.try_recv().unwrap();
        assert_eq!(response.status, 500);
        let body = response.body_rx.try_recv().unwrap();
        assert_eq!(body.as_ref(), b"Internal Server Error");
    }

    #[test]
    fn send_error_dev_mode_returns_traceback_body() {
        let (mut slot, mut rx) = make_slot_send(true);
        let traceback = "Traceback (most recent call last):\n  NameError: x\n".to_owned();
        slot.send_error(traceback);

        let mut response = rx.try_recv().unwrap();
        assert_eq!(response.status, 500);
        let body = response.body_rx.try_recv().unwrap();
        let body_str = std::str::from_utf8(body.as_ref()).unwrap();
        assert!(body_str.contains("Traceback"));
        assert!(body_str.contains("NameError"));
    }

    #[test]
    fn send_error_without_response_tx_does_not_panic() {
        let (mut slot, _rx) = make_slot_send(false);
        drop(slot.response_tx.take());
        slot.send_error("some error".to_owned());
    }
}
