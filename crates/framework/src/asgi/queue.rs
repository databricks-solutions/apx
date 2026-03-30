//! Request queue exposed to the Python asyncio thread.
//!
//! [`RequestQueue`] wraps the inbound crossbeam receiver and builds
//! `(scope, receive, send)` tuples on each `try_recv()`. The scope dict
//! is built via `scope_from_template` (the same optimized path used by
//! the existing dispatch), and runs entirely on Thread 2 (100% GIL).

use crate::asgi::scope::{
    ResolvedAwaitable, ScopeInterns, build_receive_template, scope_from_template,
};
use crate::asgi::slot_receive::SlotReceive;
use crate::asgi::slot_send::SlotSend;
use crate::io::channel::{InboundChannel, RequestSlot, Wakeup};
use crate::transport::types::{BodyStream, InboundRequest, TransportKind};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

// ── RequestQueue ─────────────────────────────────────────────────────────

/// Python-visible request queue for the 2-thread dispatch pipeline.
///
/// Created once per worker, passed to `install_dispatch()` in Python.
/// `try_recv()` is called from the `_on_readable` callback whenever
/// the wakeup pipe signals new requests.
#[pyclass(module = "apx._core")]
pub struct RequestQueue {
    inbound_rx: crossbeam_channel::Receiver<RequestSlot>,
    wakeup: Arc<Wakeup>,
    scope_interns: Arc<ScopeInterns>,
    receive_template: Py<PyDict>,
    resolved: Py<ResolvedAwaitable>,
}

crate::opaque_debug!(RequestQueue);

impl RequestQueue {
    /// Create a new request queue from the inbound channel and scope interns.
    ///
    /// Must be called with the GIL held (needs `py` for template construction).
    pub fn new(
        py: Python<'_>,
        inbound: &InboundChannel,
        wakeup: Arc<Wakeup>,
        scope_interns: Arc<ScopeInterns>,
    ) -> PyResult<Self> {
        let receive_template = build_receive_template(py)?;
        let resolved = Py::new(py, ResolvedAwaitable)?;
        Ok(Self {
            inbound_rx: inbound.receiver().clone(),
            wakeup,
            scope_interns,
            receive_template,
            resolved,
        })
    }
}

#[pymethods]
impl RequestQueue {
    /// Try to receive one request, returning `(scope, receive, send)` or `None`.
    ///
    /// Called from the asyncio `_on_readable` callback. Non-blocking —
    /// returns `None` immediately when the queue is empty, clearing the
    /// wakeup coalescing flag so the next `signal()` writes a fresh byte.
    fn try_recv<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, pyo3::types::PyTuple>>> {
        use crate::telemetry::dispatch_metrics;

        dispatch_metrics::record_queue_depth(self.inbound_rx.len() as f64);

        let slot = match self.inbound_rx.try_recv() {
            Ok(slot) => slot,
            Err(
                crossbeam_channel::TryRecvError::Empty
                | crossbeam_channel::TryRecvError::Disconnected,
            ) => {
                // Clear the coalescing flag so the next signal() writes a
                // fresh wakeup byte.  We must re-check the channel AFTER
                // clearing: a signal() racing between our first try_recv
                // (Empty) and this drain() would lose the CAS (pending was
                // still true) and never write a byte.  The re-check picks
                // up that orphaned item.
                self.wakeup.drain();
                match self.inbound_rx.try_recv() {
                    Ok(slot) => slot,
                    Err(_) => return Ok(None),
                }
            }
        };

        dispatch_metrics::record_pickup_delay(slot.created_at.elapsed().as_micros() as f64);

        self.materialize(py, slot).map(Some)
    }
}

impl RequestQueue {
    /// Build `(scope, receive, send)` Python tuple from a pure-Rust `RequestSlot`.
    fn materialize<'py>(
        &self,
        py: Python<'py>,
        slot: RequestSlot,
    ) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
        use crate::telemetry::{dispatch_metrics, timed};

        timed!(dispatch_metrics::record_materialize, {
            if let Some(ref ctx) = slot.trace_context {
                crate::telemetry::context::set_python_context(py, ctx)?;
            }

            let request = slot_to_inbound_request(&slot);
            let scope = scope_from_template(
                py,
                &self.scope_interns.scope_template,
                &request,
                None,
                &self.scope_interns,
            )?;

            let receive = SlotReceive::new(slot.body, self.receive_template.clone_ref(py));
            let receive_obj = Py::new(py, receive)?.into_bound(py).into_any();

            let send = SlotSend::new(slot.response_tx, self.resolved.clone_ref(py));
            let send_obj = Py::new(py, send)?.into_bound(py).into_any();

            let scope_any = scope.into_bound(py).into_any();
            pyo3::types::PyTuple::new(py, [scope_any, receive_obj, send_obj])
        })
    }
}

/// Build a temporary [`InboundRequest`] from a [`RequestSlot`] for
/// `scope_from_template`. The body is already consumed so we pass `Empty`.
fn slot_to_inbound_request(slot: &RequestSlot) -> InboundRequest {
    InboundRequest::new(
        slot.method.clone(),
        slot.path.clone(),
        slot.query_string.clone(),
        slot.headers.clone(),
        BodyStream::Empty,
        slot.protocol,
        TransportKind::Tcp,
        slot.client_addr,
        slot.server_addr,
        Vec::new(),
        http::Extensions::new(),
    )
}
