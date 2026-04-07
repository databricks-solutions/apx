//! HTTP/1.1 connection handler for asyncio transport/protocol.
//!
//! Implements the asyncio Protocol interface using Rust-accelerated
//! parsing, scope building, and response writing.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use crate::asgi::scope::{ResolvedAwaitableWithValue, ScopeInterns};
use crate::telemetry::dispatch_metrics;
use crate::transport::types::ProtocolVersion;

use super::parser::{HttpVersion, ParsedHead, ParsedRequest, RequestParser};
use super::writer::RustResponseWriter;

/// Maximum concurrent in-flight requests per protocol instance.
const MAX_CONCURRENT: u32 = 256;

/// Seconds of idle time before closing a keep-alive connection.
const KEEPALIVE_TIMEOUT_S: f64 = 5.0;

/// Shared state for all protocol instances on this worker.
struct ProtocolShared {
    on_request: Py<PyAny>,
    on_ws_connect: Option<Py<PyAny>>,
    interns: ScopeInterns,
    server_host: String,
    server_port: u16,
    active_requests: AtomicU32,
}

/// RAII guard for a concurrency slot in `active_requests`.
///
/// Increments the counter on [`acquire`] and decrements on [`Drop`].
/// This guarantees the counter is always released — even if the ASGI
/// app never calls `send`, the handler raises, or the connection
/// closes mid-request.
///
/// The [`release`] method allows explicit decrement (e.g. in
/// `OnRequestComplete.__call__`) while making `Drop` a no-op.
struct RequestSlot {
    shared: Arc<ProtocolShared>,
    released: bool,
}

impl RequestSlot {
    /// Try to acquire a concurrency slot. Returns `None` if the
    /// worker is at `MAX_CONCURRENT` — the caller should send 503.
    fn acquire(shared: &Arc<ProtocolShared>) -> Option<Self> {
        let active = shared.active_requests.fetch_add(1, Ordering::Relaxed);
        if active >= MAX_CONCURRENT {
            shared.active_requests.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        dispatch_metrics::inc_active_requests();
        crate::telemetry::http::inc_active_requests();
        Some(Self {
            shared: Arc::clone(shared),
            released: false,
        })
    }

    /// Explicitly release the slot. Subsequent calls and `Drop` are no-ops.
    fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.shared.active_requests.fetch_sub(1, Ordering::Relaxed);
            dispatch_metrics::dec_active_requests();
            crate::telemetry::http::dec_active_requests();
        }
    }
}

impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.release();
    }
}

/// Factory that creates [`RustProtocol`] instances for `loop.create_server()`.
///
/// Holds shared worker state (interns, dispatch callback, concurrency limit).
/// Created in Rust (worker init), passed to Python as a callable.
#[pyclass(module = "apx._core")]
pub struct ProtocolFactory {
    shared: Arc<ProtocolShared>,
}

crate::opaque_debug!(ProtocolFactory);

impl ProtocolFactory {
    /// Create a factory with shared worker state (Rust-side constructor).
    pub fn new(
        on_request: Py<PyAny>,
        on_ws_connect: Option<Py<PyAny>>,
        interns: ScopeInterns,
        server_host: String,
        server_port: u16,
    ) -> Self {
        Self {
            shared: Arc::new(ProtocolShared {
                on_request,
                on_ws_connect,
                interns,
                server_host,
                server_port,
                active_requests: AtomicU32::new(0),
            }),
        }
    }
}

#[pymethods]
impl ProtocolFactory {
    /// Called by asyncio as the protocol factory (`loop.create_server(factory)`).
    fn __call__(&self, py: Python<'_>) -> PyResult<Py<RustProtocol>> {
        Py::new(
            py,
            RustProtocol {
                transport: None,
                parser: RequestParser::new(),
                shared: Arc::clone(&self.shared),
                client_addr: None,
                keepalive_handle: None,
                ws_bridge: None,
            },
        )
    }
}

/// HTTP/1.1 protocol for asyncio `loop.create_server()`.
///
/// Plugs into asyncio's transport/protocol layer. Parses HTTP
/// requests in Rust and dispatches to a Python callback.
#[pyclass(module = "apx._core")]
pub struct RustProtocol {
    transport: Option<Py<PyAny>>,
    parser: RequestParser,
    shared: Arc<ProtocolShared>,
    client_addr: Option<SocketAddr>,
    keepalive_handle: Option<Py<PyAny>>,
    /// Active WebSocket bridge — when set, `data_received` forwards
    /// raw bytes here instead of parsing HTTP.
    ws_bridge: Option<Py<PyAny>>,
}

crate::opaque_debug!(RustProtocol);

#[pymethods]
impl RustProtocol {
    /// Called by asyncio when a connection is established.
    fn connection_made(&mut self, py: Python<'_>, transport: Py<PyAny>) -> PyResult<()> {
        self.client_addr = extract_peer_addr(py, &transport);
        self.transport = Some(transport);
        dispatch_metrics::inc_connections();
        Ok(())
    }

    /// Called by asyncio when data is received on the connection.
    ///
    /// The borrow_mut is held ONLY for pure-Rust operations (parse,
    /// timer handle read). It is dropped BEFORE any Python calls
    /// (cancel, close, import) to prevent PyO3 BorrowError when
    /// uvloop re-enters via `pause_writing` during `transport.write`.
    fn data_received(slf: &Bound<'_, Self>, py: Python<'_>, data: &[u8]) -> PyResult<()> {
        let py_self = slf.clone().unbind();

        // WebSocket fast path: if a bridge is active, forward raw bytes
        // to the wsproto parser instead of the HTTP parser.
        {
            let this = py_self.borrow(py);
            if let Some(bridge) = &this.ws_bridge {
                let py_bytes = PyBytes::new(py, data);
                bridge.call_method1(py, pyo3::intern!(py, "feed_data"), (py_bytes,))?;
                return Ok(());
            }
        }

        // Borrow mutably only for pure-Rust work (parse + extract handles).
        let (feed_result, keepalive_handle, error_transport) = {
            let mut this = py_self.borrow_mut(py);
            let keepalive = this.keepalive_handle.take();
            let result =
                crate::telemetry::timed!(dispatch_metrics::record_parse, this.parser.feed(data));
            let err_transport = if result.is_err() {
                this.transport.as_ref().map(|t| t.clone_ref(py))
            } else {
                None
            };
            (result, keepalive, err_transport)
            // borrow_mut dropped here — BEFORE any Python calls.
        };

        // All Python calls below — borrow is released to avoid PyO3 BorrowError.
        if let Some(handle) = keepalive_handle {
            let _ = handle.call_method0(py, pyo3::intern!(py, "cancel"));
        }

        let requests = match feed_result {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    name: "apx.protocol.parse_error",
                    error = %e,
                    "malformed HTTP request"
                );
                if let Some(transport) = error_transport {
                    let _ = transport_write(py, &transport, REJECT_BAD_REQUEST);
                    let _ = transport.call_method0(py, pyo3::intern!(py, "close"));
                }
                return Ok(());
            }
        };

        let event_loop = py
            .import("asyncio")?
            .call_method0(pyo3::intern!(py, "get_running_loop"))?
            .unbind();

        for parsed in requests {
            Self::dispatch_request_inner(py, &py_self, &event_loop, parsed)?;
        }
        Ok(())
    }

    /// Called by asyncio when the peer sends EOF.
    #[expect(clippy::unused_self, reason = "required by asyncio protocol interface")]
    fn eof_received(&self) -> bool {
        false
    }

    /// Called by asyncio when the connection is lost.
    fn connection_lost(&mut self, py: Python<'_>, _exc: Option<&Bound<'_, PyAny>>) {
        self.cancel_keepalive_timer(py);
        if let Some(bridge) = self.ws_bridge.take() {
            let _ = bridge.call_method0(py, pyo3::intern!(py, "connection_lost"));
        }
        self.transport = None;
        self.parser.reset();
        dispatch_metrics::dec_connections();
    }
    /// Flow control callback from uvloop. Required by the asyncio
    /// protocol interface; uvloop logs an error if the method is missing.
    #[expect(clippy::unused_self, reason = "required by asyncio protocol interface")]
    fn pause_writing(&self) {}

    /// Counterpart to `pause_writing` — write buffer drained.
    #[expect(clippy::unused_self, reason = "required by asyncio protocol interface")]
    fn resume_writing(&self) {}

    /// Close the connection if idle (no active requests).
    ///
    /// Called by the event loop's `call_later` as the keep-alive timeout.
    fn close_idle(&mut self, py: Python<'_>) {
        if self.shared.active_requests.load(Ordering::Relaxed) == 0
            && self.ws_bridge.is_none()
            && let Some(transport) = &self.transport
        {
            let _ = transport.call_method0(py, pyo3::intern!(py, "close"));
        }
    }

    /// Store a WebSocket bridge reference on this protocol.
    ///
    /// Called from the Python-side WebSocket handler after upgrade.
    /// Subsequent `data_received` calls will route to the bridge.
    fn set_ws_bridge(&mut self, bridge: Py<PyAny>) {
        self.ws_bridge = Some(bridge);
    }
}

impl RustProtocol {
    fn cancel_keepalive_timer(&mut self, py: Python<'_>) {
        if let Some(handle) = self.keepalive_handle.take() {
            let _ = handle.call_method0(py, pyo3::intern!(py, "cancel"));
        }
    }

    fn dispatch_request_inner(
        py: Python<'_>,
        py_self: &Py<Self>,
        event_loop: &Py<PyAny>,
        parsed: ParsedRequest,
    ) -> PyResult<()> {
        let this = py_self.borrow(py);
        let t_dispatch = Instant::now();
        let Some(transport) = &this.transport else {
            return Ok(());
        };

        let Some(slot) = RequestSlot::acquire(&this.shared) else {
            transport_write(py, transport, REJECT_OVERLOADED)?;
            return Ok(());
        };

        // Release the borrow before calling into Python.
        // `slot` owns the concurrency decrement — if anything below
        // fails or the writer is GC'd without calling send, `Drop`
        // fires and the counter is released automatically.
        drop(this);

        let result = Self::dispatch_body(py, py_self, event_loop, parsed, t_dispatch, slot);

        if let Err(e) = result {
            // `slot` was either moved into OnRequestComplete (and will
            // be released via Drop when OnRequestComplete is GC'd) or
            // it was dropped by dispatch_body on error (Drop fires).
            // Either way, the counter is handled. Just resume reading.
            if let Ok(this) = py_self.try_borrow(py)
                && let Some(transport) = &this.transport
            {
                let _ = transport.call_method0(py, pyo3::intern!(py, "resume_reading"));
            }
            tracing::debug!(
                name: "apx.protocol.dispatch_error",
                error = %e,
                "request dispatch failed"
            );
            return Err(e);
        }
        Ok(())
    }

    /// Inner dispatch body. The `slot` RAII guard ensures the
    /// active_requests counter is decremented if this function errors
    /// before transferring the slot into `OnRequestComplete`.
    fn dispatch_body(
        py: Python<'_>,
        py_self: &Py<Self>,
        event_loop: &Py<PyAny>,
        parsed: ParsedRequest,
        t_dispatch: Instant,
        slot: RequestSlot,
    ) -> PyResult<()> {
        // WebSocket upgrade: detect and dispatch separately.
        if is_websocket_upgrade(&parsed.head.headers) {
            return Self::dispatch_websocket(py, py_self, parsed, slot);
        }

        // Borrow briefly to extract what we need, then release before
        // calling into Python (transport.write may trigger pause_writing
        // which needs to borrow &self).
        let (transport, shared, on_request, client_addr) = {
            let this = py_self.borrow(py);
            let Some(transport) = &this.transport else {
                return Ok(());
            };
            let t = transport.clone_ref(py);
            let s = Arc::clone(&this.shared);
            let o = this.shared.on_request.clone_ref(py);
            let c = this.client_addr;
            (t, s, o, c)
        };
        // Borrow released — safe to call Python methods that may
        // re-enter RustProtocol (e.g. pause_writing via transport.write).

        transport.call_method0(py, pyo3::intern!(py, "pause_reading"))?;

        let (request_id, has_request_id) = resolve_request_id(&parsed.head.headers);

        let scope = crate::telemetry::timed!(
            dispatch_metrics::record_scope_build,
            build_scope_from_parsed(
                py,
                &parsed,
                &shared.interns,
                &shared.server_host,
                shared.server_port,
                client_addr,
                &request_id,
                has_request_id,
            )?
        );

        let receive = crate::telemetry::timed!(
            dispatch_metrics::record_receive_build,
            HttpReceive::new(
                py,
                parsed.body,
                Some(transport.clone_ref(py)),
                parsed.head.expect_continue,
            )?
        );

        let method = parsed.head.method.as_str().to_owned();
        let path = parsed.head.path;

        let (request_span, trace_ctx) =
            crate::telemetry::http::begin_request_span(&request_id, &method, &path);
        crate::telemetry::context::set_python_context(py, &trace_ctx)?;

        let on_complete = OnRequestComplete::create(
            py,
            transport.clone_ref(py),
            t_dispatch,
            method,
            path,
            request_span,
            py_self.clone_ref(py),
            event_loop.clone_ref(py),
            slot,
        )?;

        let send = RustResponseWriter::new(py, transport, Some(on_complete.into_any()))?;

        on_request.call1(py, (scope, receive, send))?;
        Ok(())
    }

    /// Dispatch a WebSocket upgrade request to the Python bridge.
    ///
    /// Builds the ASGI WebSocket scope, writes the 101 Switching
    /// Protocols response, creates a `WebSocketBridge`, and stores
    /// it so subsequent `data_received` calls route to it.
    fn dispatch_websocket(
        py: Python<'_>,
        py_self: &Py<Self>,
        parsed: ParsedRequest,
        _slot: RequestSlot,
    ) -> PyResult<()> {
        let (transport, shared, client_addr) = {
            let this = py_self.borrow(py);
            let Some(transport) = &this.transport else {
                return Ok(());
            };
            (
                transport.clone_ref(py),
                Arc::clone(&this.shared),
                this.client_addr,
            )
        };

        let Some(on_ws_connect) = &shared.on_ws_connect else {
            // No WS handler registered — reject with 400.
            transport_write(py, &transport, REJECT_BAD_REQUEST)?;
            return Ok(());
        };

        // Build WebSocket ASGI scope.
        let scope = PyDict::new(py);
        scope.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "websocket"))?;
        let asgi_dict = PyDict::new(py);
        asgi_dict.set_item(pyo3::intern!(py, "version"), "3.0")?;
        asgi_dict.set_item(pyo3::intern!(py, "spec_version"), "2.4")?;
        scope.set_item(pyo3::intern!(py, "asgi"), asgi_dict)?;
        scope.set_item(pyo3::intern!(py, "http_version"), "1.1")?;
        scope.set_item(pyo3::intern!(py, "scheme"), "ws")?;
        scope.set_item(pyo3::intern!(py, "path"), &parsed.head.path)?;
        scope.set_item(
            pyo3::intern!(py, "raw_path"),
            PyBytes::new(py, parsed.head.path.as_bytes()),
        )?;
        scope.set_item(
            pyo3::intern!(py, "query_string"),
            PyBytes::new(py, &parsed.head.query_string),
        )?;
        scope.set_item(pyo3::intern!(py, "root_path"), "")?;

        // Build headers list (same format as HTTP scope).
        let header_list = PyList::empty(py);
        for (name, value) in &parsed.head.headers {
            let tuple = PyTuple::new(
                py,
                [
                    PyBytes::new(py, name).as_any(),
                    PyBytes::new(py, value).as_any(),
                ],
            )?;
            header_list.append(tuple)?;
        }
        scope.set_item(pyo3::intern!(py, "headers"), header_list)?;

        // Client/server addresses.
        if let Some(addr) = client_addr {
            scope.set_item(
                pyo3::intern!(py, "client"),
                (addr.ip().to_string(), addr.port()),
            )?;
        } else {
            scope.set_item(pyo3::intern!(py, "client"), py.None())?;
        }
        scope.set_item(
            pyo3::intern!(py, "server"),
            (&*shared.server_host, shared.server_port),
        )?;

        // Extract subprotocols from Sec-WebSocket-Protocol header.
        let subprotocols = PyList::empty(py);
        for (name, value) in &parsed.head.headers {
            if name.eq_ignore_ascii_case(b"sec-websocket-protocol")
                && let Ok(s) = std::str::from_utf8(value)
            {
                for proto in s.split(',') {
                    subprotocols.append(proto.trim())?;
                }
            }
        }
        scope.set_item(pyo3::intern!(py, "subprotocols"), subprotocols)?;
        scope.set_item(pyo3::intern!(py, "state"), PyDict::new(py))?;

        // Extract Sec-WebSocket-Key for the 101 response.
        let mut ws_key = String::new();
        for (name, value) in &parsed.head.headers {
            if name.eq_ignore_ascii_case(b"sec-websocket-key")
                && let Ok(s) = std::str::from_utf8(value)
            {
                s.clone_into(&mut ws_key);
            }
        }

        // Call the Python-side WebSocket handler.
        // It builds the 101 response, creates the bridge, and stores
        // a reference back on this protocol via set_ws_bridge().
        let scope_obj = scope.unbind().into_any();
        on_ws_connect.call1(py, (scope_obj, &transport, &ws_key, py_self))?;
        Ok(())
    }
}

// ── WebSocket upgrade detection ─────────────────────────────────

/// Check if a parsed HTTP request is a WebSocket upgrade.
fn is_websocket_upgrade(headers: &[(Bytes, Bytes)]) -> bool {
    let mut has_upgrade = false;
    let mut has_connection = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(b"upgrade") && value.eq_ignore_ascii_case(b"websocket") {
            has_upgrade = true;
        }
        if name.eq_ignore_ascii_case(b"connection") {
            for part in value.split(|&b| b == b',') {
                let trimmed = part
                    .iter()
                    .copied()
                    .skip_while(u8::is_ascii_whitespace)
                    .collect::<Vec<_>>();
                if trimmed.eq_ignore_ascii_case(b"upgrade") {
                    has_connection = true;
                }
            }
        }
    }
    has_upgrade && has_connection
}

/// Callback invoked when a response is fully written.
///
/// Resumes reading on the transport, decrements the active count,
/// records handler_wait duration, emits `http.server.request.duration`,
/// and ends the OTEL request span.
#[pyclass(module = "apx._core")]
struct OnRequestComplete {
    transport: Py<PyAny>,
    dispatch_start: Instant,
    method: String,
    path: String,
    request_span: tracing::Span,
    protocol: Py<RustProtocol>,
    event_loop: Py<PyAny>,
    /// RAII concurrency slot — `Drop` decrements `active_requests`
    /// if `__call__` was never invoked (e.g. app never called `send`,
    /// or the `RustResponseWriter` was GC'd without completion).
    slot: RequestSlot,
}

crate::opaque_debug!(OnRequestComplete);

impl OnRequestComplete {
    #[expect(
        clippy::too_many_arguments,
        reason = "all fields needed for completion callback; struct builder would add overhead"
    )]
    fn create(
        py: Python<'_>,
        transport: Py<PyAny>,
        dispatch_start: Instant,
        method: String,
        path: String,
        request_span: tracing::Span,
        protocol: Py<RustProtocol>,
        event_loop: Py<PyAny>,
        slot: RequestSlot,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                transport,
                dispatch_start,
                method,
                path,
                request_span,
                protocol,
                event_loop,
                slot,
            },
        )
    }
}

#[pymethods]
impl OnRequestComplete {
    fn __call__(&mut self, py: Python<'_>, status: u16) -> PyResult<()> {
        let elapsed = self.dispatch_start.elapsed();

        {
            let _guard = self.request_span.enter();
            dispatch_metrics::record_handler_wait(elapsed.as_micros() as f64);
            dispatch_metrics::record_dispatch_total(elapsed.as_micros() as f64);

            crate::telemetry::http::record_duration(
                elapsed.as_secs_f64(),
                &self.method,
                "http",
                status,
                &self.path,
                None,
            );

            crate::telemetry::http::finish_request_span(&self.request_span, status);
        }

        // Resume reading (may fail if connection closed — that's OK).
        let resume_result = self
            .transport
            .call_method0(py, pyo3::intern!(py, "resume_reading"));
        if let Err(e) = resume_result {
            tracing::debug!(
                name: "apx.protocol.resume_reading_failed",
                error = %e,
                "resume_reading failed (connection likely closed)"
            );
        }

        // Release the concurrency slot explicitly. This makes the
        // Drop a no-op — the counter won't be double-decremented.
        self.slot.release();

        if let Ok(close_idle) = self.protocol.getattr(py, pyo3::intern!(py, "close_idle"))
            && let Ok(handle) = self.event_loop.call_method1(
                py,
                pyo3::intern!(py, "call_later"),
                (KEEPALIVE_TIMEOUT_S, close_idle),
            )
            && let Ok(mut proto) = self.protocol.try_borrow_mut(py)
        {
            proto.keepalive_handle = Some(handle);
        }

        Ok(())
    }
}

// ── HttpReceive ──────────────────────────────────────────────────────

/// ASGI `receive` callable for non-streaming HTTP requests.
///
/// First call returns the request body immediately via
/// `ResolvedAwaitableWithValue`. Subsequent calls return a pending
/// future (disconnect sentinel). Handles `Expect: 100-continue` by
/// writing the informational response before delivering the body.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct HttpReceive {
    body: std::sync::Mutex<Option<Bytes>>,
    transport: Option<Py<PyAny>>,
    expect_continue: bool,
}

crate::opaque_debug!(HttpReceive);

impl HttpReceive {
    fn new(
        py: Python<'_>,
        body: Bytes,
        transport: Option<Py<PyAny>>,
        expect_continue: bool,
    ) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                body: std::sync::Mutex::new(Some(body)),
                transport,
                expect_continue,
            },
        )
    }
}

#[pymethods]
impl HttpReceive {
    fn __call__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let body = self
            .body
            .lock()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?
            .take();

        if let Some(b) = body {
            if self.expect_continue
                && let Some(transport) = &self.transport
            {
                let _ = transport_write(py, transport, INFORMATIONAL_CONTINUE);
            }

            let event = PyDict::new(py);
            event.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))?;
            event.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, &b))?;
            event.set_item(pyo3::intern!(py, "more_body"), false)?;
            let awaitable = Py::new(
                py,
                ResolvedAwaitableWithValue::new(event.into_any().unbind()),
            )?;
            Ok(awaitable.into_any())
        } else {
            let fut = py
                .import("asyncio")?
                .call_method0(pyo3::intern!(py, "get_running_loop"))?
                .call_method0(pyo3::intern!(py, "create_future"))?;
            Ok(fut.unbind())
        }
    }
}

// ── Scope building ──────────────────────────────────────────────────────

/// Build an ASGI HTTP scope dict directly from [`ParsedRequest`].
///
/// Bypasses `ScopeSource` trait and `HeaderMap` — works with raw byte
/// pairs from the parser, avoiding the intermediate allocation.
#[expect(
    clippy::too_many_arguments,
    reason = "scope construction needs all ASGI fields; splitting would fragment the hot path"
)]
fn build_scope_from_parsed(
    py: Python<'_>,
    parsed: &ParsedRequest,
    interns: &ScopeInterns,
    server_host: &str,
    server_port: u16,
    client_addr: Option<SocketAddr>,
    request_id: &str,
    has_request_id: bool,
) -> PyResult<Py<PyDict>> {
    let scope = interns
        .scope_template
        .bind(py)
        .call_method0(pyo3::intern!(py, "copy"))?
        .cast_into::<PyDict>()?;

    let version = match parsed.head.version {
        HttpVersion::Http10 => ProtocolVersion::Http10,
        HttpVersion::Http11 => ProtocolVersion::Http11,
    };
    if version != ProtocolVersion::Http11 {
        scope.set_item(
            interns.keys.http_version.bind(py),
            interns.versions.get(py, version),
        )?;
    }

    scope.set_item(interns.keys.method.bind(py), parsed.head.method.as_str())?;
    scope.set_item(
        interns.keys.path.bind(py),
        percent_decode(&parsed.head.path).as_ref(),
    )?;
    scope.set_item(
        interns.keys.raw_path.bind(py),
        PyBytes::new(py, parsed.head.path.as_bytes()),
    )?;
    scope.set_item(
        interns.keys.query_string.bind(py),
        PyBytes::new(py, &parsed.head.query_string),
    )?;

    set_headers_from_parsed(
        py,
        &scope,
        &parsed.head,
        interns,
        request_id,
        has_request_id,
    )?;
    set_addresses(py, &scope, interns, server_host, server_port, client_addr)?;
    scope.set_item(
        interns.keys.path_params.bind(py),
        interns.empty_dict.bind(py),
    )?;
    scope.set_item(interns.keys.state.bind(py), PyDict::new(py))?;

    Ok(scope.unbind())
}

/// Extract existing `x-request-id` from headers or generate a UUID v4.
///
/// Returns `(request_id, has_request_id)` so the scope builder can
/// skip a second header scan.
fn resolve_request_id(headers: &[(Bytes, Bytes)]) -> (String, bool) {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(b"x-request-id")
            && let Ok(s) = std::str::from_utf8(value)
        {
            return (s.to_owned(), true);
        }
    }
    (generate_uuid_v4(), false)
}

/// Set headers list from raw byte pairs (no `HeaderMap` intermediary).
///
/// Prepends `x-request-id` if not already present in the request.
fn set_headers_from_parsed(
    py: Python<'_>,
    scope: &Bound<'_, PyDict>,
    head: &ParsedHead,
    interns: &ScopeInterns,
    request_id: &str,
    has_request_id: bool,
) -> PyResult<()> {
    let extra_cap = usize::from(!has_request_id);
    let mut pairs: Vec<Bound<'_, PyAny>> = Vec::with_capacity(head.headers.len() + extra_cap);

    if !has_request_id {
        let id_name = PyBytes::new(py, b"x-request-id");
        let id_value = PyBytes::new(py, request_id.as_bytes());
        let pair = PyTuple::new(py, [id_name.into_any(), id_value.into_any()])?;
        pairs.push(pair.into_any());
    }

    for (name, value) in &head.headers {
        let n = intern_header_name(py, name, interns);
        let v = PyBytes::new(py, value);
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        pairs.push(pair.into_any());
    }
    let headers_list = PyList::new(py, &pairs)?;
    scope.set_item(interns.keys.headers.bind(py), headers_list)?;
    Ok(())
}

/// Generate a UUID v4 string (random, RFC 4122 variant 1).
fn generate_uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Try to use the header intern cache, falling back to `PyBytes::new`.
fn intern_header_name<'py>(
    py: Python<'py>,
    name: &Bytes,
    interns: &ScopeInterns,
) -> Bound<'py, PyBytes> {
    let name_lower = name.to_ascii_lowercase();
    if let Some(cached) = interns.headers.map.get(name_lower.as_slice()) {
        return cached.bind(py).clone();
    }
    PyBytes::new(py, &name_lower)
}

/// Set server and client address tuples.
fn set_addresses(
    py: Python<'_>,
    scope: &Bound<'_, PyDict>,
    interns: &ScopeInterns,
    _server_host: &str,
    _server_port: u16,
    client_addr: Option<SocketAddr>,
) -> PyResult<()> {
    scope.set_item(interns.keys.server.bind(py), interns.server_tuple.bind(py))?;
    match client_addr {
        Some(addr) => {
            scope.set_item(
                interns.keys.client.bind(py),
                (addr.ip().to_string(), addr.port()),
            )?;
        }
        None => scope.set_item(interns.keys.client.bind(py), py.None())?,
    }
    Ok(())
}

/// Percent-decode a URL path.
fn percent_decode(input: &str) -> Cow<'_, str> {
    percent_encoding::percent_decode_str(input).decode_utf8_lossy()
}

/// Extract the peer address from an asyncio transport.
fn extract_peer_addr(py: Python<'_>, transport: &Py<PyAny>) -> Option<SocketAddr> {
    let peername = transport
        .call_method1(
            py,
            pyo3::intern!(py, "get_extra_info"),
            (pyo3::intern!(py, "peername"),),
        )
        .ok()?;
    if peername.is_none(py) {
        return None;
    }
    let bound = peername.bind(py);
    let tuple: &Bound<'_, PyTuple> = bound.cast().ok()?;
    let host: String = tuple.get_item(0).ok()?.extract().ok()?;
    let port: u16 = tuple.get_item(1).ok()?.extract().ok()?;
    let ip: std::net::IpAddr = host.parse().ok()?;
    Some(SocketAddr::new(ip, port))
}

// ── Pre-built error responses (sans-I/O: pure data, no transport) ───

/// Sent when the parser cannot decode the incoming bytes as valid HTTP.
const REJECT_BAD_REQUEST: &[u8] = b"HTTP/1.1 400 Bad Request\r\n\
    content-length: 11\r\n\
    content-type: text/plain\r\n\
    connection: close\r\n\
    \r\n\
    Bad Request";

/// Sent when the per-connection concurrency limit is reached.
const REJECT_OVERLOADED: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
    content-length: 19\r\n\
    content-type: text/plain\r\n\
    \r\n\
    Service Unavailable";

/// Informational response for `Expect: 100-continue`.
const INFORMATIONAL_CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";

// ── Transport I/O helper ────────────────────────────────────────────

/// Write raw bytes to an asyncio transport.
fn transport_write(py: Python<'_>, transport: &Py<PyAny>, data: &[u8]) -> PyResult<()> {
    let py_bytes = PyBytes::new(py, data);
    transport.call_method1(py, pyo3::intern!(py, "write"), (py_bytes,))?;
    Ok(())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_generate_uuid_v4_format() {
        let id = generate_uuid_v4();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
    }

    #[test]
    fn test_generate_uuid_v4_version_bits() {
        let id = generate_uuid_v4();
        let version_char = id.chars().nth(14).expect("version char");
        assert_eq!(version_char, '4', "UUID version nibble should be 4");
    }

    #[test]
    fn test_generate_uuid_v4_variant_bits() {
        let id = generate_uuid_v4();
        let variant_char = id.chars().nth(19).expect("variant char");
        assert!(
            matches!(variant_char, '8' | '9' | 'a' | 'b'),
            "UUID variant nibble should be 8/9/a/b, got {variant_char}"
        );
    }

    #[test]
    fn test_generate_uuid_v4_uniqueness() {
        let a = generate_uuid_v4();
        let b = generate_uuid_v4();
        assert_ne!(a, b);
    }
}
