//! ASGI protocol primitives backed by Rust.
//!
//! Provides `AsgiReceive`, `AsgiSend` (Python callables), `build_http_scope`,
//! and `build_ws_scope` for constructing ASGI scope dicts from [`InboundRequest`].
//!
//! These types enable Starlette's `Request`, `StreamingResponse`, and `WebSocket`
//! to work unmodified against a Rust-backed ASGI server.

use crate::protocol::http::error::AppError;
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyDictMethods, PyList, PyString, PyTuple};
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

/// ASGI protocol version string.
const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
const ASGI_SPEC_VERSION: &str = "2.4";

/// Default HTTP scheme (TLS detection is a future extension).
const DEFAULT_SCHEME: &str = "http";

/// Default WebSocket scheme.
const WS_SCHEME: &str = "ws";

// ── ScopeInterns ─────────────────────────────────────────────────────────

/// Pre-interned Python strings for ASGI scope construction.
///
/// Created once at worker startup, shared across all requests via `AppState`.
/// Eliminates ~25 transient `PyString` allocations per request.
impl std::fmt::Debug for ScopeInterns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeInterns").finish_non_exhaustive()
    }
}

pub struct ScopeInterns {
    /// Fixed keys used in every ASGI scope dict.
    pub(crate) keys: ScopeKeys,
    /// Fixed values (type strings, version strings, empty root_path).
    pub(crate) vals: ScopeValues,
    /// Cached `PyBytes` for common HTTP header names.
    pub(crate) headers: HeaderInterns,
}

/// Fixed dict keys used in ASGI scope construction.
pub struct ScopeKeys {
    pub(crate) r#type: Py<PyString>,
    pub(crate) asgi: Py<PyString>,
    pub(crate) http_version: Py<PyString>,
    pub(crate) method: Py<PyString>,
    pub(crate) path: Py<PyString>,
    pub(crate) raw_path: Py<PyString>,
    pub(crate) query_string: Py<PyString>,
    pub(crate) headers: Py<PyString>,
    pub(crate) server: Py<PyString>,
    pub(crate) client: Py<PyString>,
    pub(crate) scheme: Py<PyString>,
    pub(crate) root_path: Py<PyString>,
    pub(crate) state: Py<PyString>,
    pub(crate) path_params: Py<PyString>,
    pub(crate) app: Py<PyString>,
    pub(crate) router: Py<PyString>,
}

/// Fixed dict values used in ASGI scope construction.
pub struct ScopeValues {
    pub(crate) type_http: Py<PyString>,
    pub(crate) type_websocket: Py<PyString>,
    pub(crate) scheme_http: Py<PyString>,
    pub(crate) scheme_ws: Py<PyString>,
    pub(crate) root_path_empty: Py<PyString>,
    /// Pre-built `{"version": "3.0", "spec_version": "2.3"}` dict, shared per-request.
    pub(crate) asgi_dict: Py<PyDict>,
}

/// Common HTTP header names, ordered by frequency in typical HTTP/1.1 traffic.
const COMMON_HEADERS: &[HeaderName] = &[
    header::HOST,
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::ACCEPT,
    header::USER_AGENT,
    header::ACCEPT_ENCODING,
    header::ACCEPT_LANGUAGE,
    header::CONNECTION,
    header::CACHE_CONTROL,
    header::COOKIE,
    header::AUTHORIZATION,
    header::TRANSFER_ENCODING,
    header::CONTENT_ENCODING,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::ORIGIN,
    header::REFERER,
];

/// Pre-built `PyBytes` for common HTTP header names.
///
/// `http::HeaderName` standard constants compare by pointer, so the
/// lookup is a pointer match — not a string hash.
pub struct HeaderInterns {
    map: Vec<(HeaderName, Py<PyBytes>)>,
}

impl HeaderInterns {
    /// Create cached `PyBytes` for common header names. Call once at worker startup.
    pub fn new(py: Python<'_>) -> Self {
        let map = COMMON_HEADERS
            .iter()
            .map(|h| (h.clone(), PyBytes::new(py, h.as_str().as_bytes()).unbind()))
            .collect();
        Self { map }
    }

    /// Look up a cached `PyBytes` for this header name.
    /// Returns `None` for non-standard headers (fallback to `PyBytes::new`).
    pub fn get<'py>(&self, py: Python<'py>, name: &HeaderName) -> Option<Bound<'py, PyBytes>> {
        self.map
            .iter()
            .find(|(h, _)| h == name)
            .map(|(_, cached)| cached.bind(py).clone())
    }
}

impl ScopeInterns {
    /// Create all interned strings. Call once at worker startup with GIL held.
    pub(crate) fn new(py: Python<'_>) -> Self {
        let s = |v: &str| PyString::intern(py, v).clone().unbind();

        // Pre-build the ASGI inner dict once instead of per-request.
        let asgi_dict = PyDict::new(py);
        // These set_item calls are infallible for interned string keys/values.
        let _ = asgi_dict.set_item(s("version").bind(py), s(ASGI_VERSION).bind(py));
        let _ = asgi_dict.set_item(s("spec_version").bind(py), s(ASGI_SPEC_VERSION).bind(py));

        Self {
            keys: ScopeKeys {
                r#type: s("type"),
                asgi: s("asgi"),
                http_version: s("http_version"),
                method: s("method"),
                path: s("path"),
                raw_path: s("raw_path"),
                query_string: s("query_string"),
                headers: s("headers"),
                server: s("server"),
                client: s("client"),
                scheme: s("scheme"),
                root_path: s("root_path"),
                state: s("state"),
                path_params: s("path_params"),
                app: s("app"),
                router: s("router"),
            },
            vals: ScopeValues {
                type_http: s("http"),
                type_websocket: s("websocket"),
                scheme_http: s(DEFAULT_SCHEME),
                scheme_ws: s(WS_SCHEME),
                root_path_empty: s(""),
                asgi_dict: asgi_dict.unbind(),
            },
            headers: HeaderInterns::new(py),
        }
    }
}

/// Build the ASGI receive template dict: `{"type": "http.request", "body": b"", "more_body": false}`.
///
/// Created once at worker startup, stored in `AsgiDispatch`. Each request
/// copies this dict (inside `AsgiReceive::__call__`) and patches the `body` field.
pub fn build_receive_template(py: Python<'_>) -> Py<PyDict> {
    let dict = PyDict::new(py);
    // These set_item calls are infallible for interned string keys.
    let _ = dict.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"));
    let _ = dict.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, b""));
    let _ = dict.set_item(pyo3::intern!(py, "more_body"), false);
    dict.unbind()
}

// ── AsgiEvent ────────────────────────────────────────────────────────────

/// Parsed ASGI send event (Rust-side representation).
///
/// Pushed through a channel from [`AsgiSend`] (Python side) to the response
/// collector (Rust side) that assembles the final HTTP response or relays
/// WebSocket frames.
#[derive(Debug)]
pub enum AsgiEvent {
    /// `http.response.start` — status code and headers.
    ResponseStart {
        /// HTTP status code.
        status: u16,
        /// Response headers, built directly from Python bytes.
        headers: HeaderMap,
    },
    /// `http.response.body` — body chunk with continuation flag.
    ResponseBody {
        /// Body bytes.
        body: Bytes,
        /// Whether more body chunks follow.
        more_body: bool,
    },
    /// `websocket.accept` — server accepts the WebSocket connection.
    WsAccept {
        /// Optional subprotocol.
        subprotocol: Option<String>,
        /// Response headers as raw byte pairs.
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// `websocket.send` — server sends a frame to the client.
    WsSend {
        /// Text frame payload.
        text: Option<String>,
        /// Binary frame payload.
        bytes: Option<Vec<u8>>,
    },
    /// `websocket.close` — server closes the connection.
    WsClose {
        /// WebSocket close code (default 1000).
        code: u16,
    },
}

// ── AsgiReceive ──────────────────────────────────────────────────────────

/// ASGI `receive` callable backed by Rust.
///
/// For HTTP: first call returns `http.request` with the pre-buffered body
/// synchronously (via `ResolvedAwaitableWithValue`, no tokio task overhead).
/// Subsequent calls pend forever via `future_into_py` + `pending()`,
/// preventing Starlette's `listen_for_disconnect` from prematurely firing.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct AsgiReceive {
    body: std::sync::Mutex<Option<Bytes>>,
    receive_template: Py<PyDict>,
    disconnect_rx: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl std::fmt::Debug for AsgiReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiReceive").finish_non_exhaustive()
    }
}

impl AsgiReceive {
    /// Create for an HTTP request with a known body.
    pub fn http(
        body: Bytes,
        receive_template: Py<PyDict>,
        disconnect_rx: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(body)),
            receive_template,
            disconnect_rx: std::sync::Mutex::new(Some(disconnect_rx)),
        }
    }

    /// Create for an HTTP request with no body (GET, HEAD, DELETE).
    pub fn empty(receive_template: Py<PyDict>, disconnect_rx: oneshot::Receiver<()>) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(Bytes::new())),
            receive_template,
            disconnect_rx: std::sync::Mutex::new(Some(disconnect_rx)),
        }
    }
}

#[pymethods]
impl AsgiReceive {
    /// Python: `event = await receive()`
    ///
    /// First call: returns body synchronously via `ResolvedAwaitableWithValue`
    /// (no tokio task, no `future_into_py` overhead).
    /// Subsequent calls: pend forever via `future_into_py` + `pending()`
    /// (proper asyncio suspension for the disconnect listener).
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let taken = self
            .body
            .lock()
            .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("receive mutex poisoned"))?
            .take();

        if let Some(bytes) = taken {
            let t0 = super::bench_trace_enabled().then(std::time::Instant::now);
            let event: Bound<'_, PyDict> = self
                .receive_template
                .bind(py)
                .call_method0(pyo3::intern!(py, "copy"))?
                .cast_into()?;
            event.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, &bytes))?;
            if let Some(t0) = t0 {
                tracing::info!(
                    target: "bench_trace",
                    phase = "receive_dict_copy",
                    elapsed_us = t0.elapsed().as_micros(),
                    body_len = bytes.len(),
                );
            }
            let event = event.unbind().into_any();
            Py::new(py, ResolvedAwaitableWithValue { value: Some(event) })
                .map(|obj| obj.into_bound(py).into_any())
        } else {
            // Deliver http.disconnect when the response stream ends.
            let (future, resolve_tx) = crate::scheduler::primitives::Future::with_channel();
            let py_future = Py::new(py, future)?;

            let maybe_disconnect = self
                .disconnect_rx
                .lock()
                .map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("disconnect mutex poisoned")
                })?
                .take();
            if let Some(disconnect_rx) = maybe_disconnect {
                let disconnect_type = pyo3::intern!(py, "http.disconnect").clone().unbind();
                let type_key = pyo3::intern!(py, "type").clone().unbind();
                crate::scheduler::with_tokio_handle(|handle| {
                    handle.spawn(async move {
                        let _ = disconnect_rx.await;
                        Python::attach(|py| {
                            let event = PyDict::new(py);
                            event.set_item(&type_key, &disconnect_type).ok();
                            let _ = resolve_tx.send(event.unbind().into_any());
                        });
                    });
                })
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for disconnect watch",
                    )
                })?;
            }
            // If disconnect_rx already taken (third+ call), the Future's sender drops
            // → Future raises RuntimeError. Only one receive() should block for disconnect.

            Ok(py_future.into_bound(py).into_any())
        }
    }
}

// ── ResolvedAwaitable ─────────────────────────────────────────────────────

/// Zero-overhead Python awaitable that completes immediately.
///
/// Used by buffered `AsgiSend` to avoid `pyo3_async_runtimes::future_into_py`
/// and its tokio task overhead. Implements the Python iterator protocol
/// so `await resolved_awaitable` returns `None` with no scheduling.
#[pyclass(module = "apx._core", freelist = 128)]
struct ResolvedAwaitable;

#[pymethods]
impl ResolvedAwaitable {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[expect(clippy::unused_self, reason = "required by Python iterator protocol")]
    fn __next__(&self) -> Option<Py<PyAny>> {
        None // StopIteration — completes immediately
    }
}

/// Zero-overhead Python awaitable that completes immediately with a value.
///
/// Used by `AsgiReceive` to return the receive dict without
/// `future_into_py` (which requires a tokio runtime, unavailable on
/// `spawn_blocking` threads).
#[pyclass(module = "apx._core", freelist = 64)]
struct ResolvedAwaitableWithValue {
    value: Option<Py<PyAny>>,
}

#[pymethods]
impl ResolvedAwaitableWithValue {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(&mut self) -> PyResult<Py<PyAny>> {
        // Raise StopIteration(value) — this is how Python awaitables return results.
        let val = self
            .value
            .take()
            .unwrap_or_else(|| Python::attach(|py| py.None()));
        Err(pyo3::exceptions::PyStopIteration::new_err((val,)))
    }
}

// ── AsgiSend ─────────────────────────────────────────────────────────────

/// Channel capacity for streaming body chunks after the first.
const STREAM_CHANNEL_CAPACITY: usize = 8;

/// Internal state for [`AsgiSend`] — HTTP vs WebSocket mode.
enum SendInner {
    /// HTTP mode — accumulates response, sends via oneshot.
    Http {
        status: Option<u16>,
        headers: Option<HeaderMap>,
        response_tx: Option<oneshot::Sender<Result<OutboundResponse, AppError>>>,
        disconnect_tx: Option<oneshot::Sender<()>>,
        stream_tx: Option<mpsc::Sender<AsgiEvent>>,
    },
    /// WebSocket mode — forwards events via mpsc (unchanged).
    Ws { tx: mpsc::Sender<AsgiEvent> },
}

/// ASGI `send` callable backed by Rust.
///
/// In HTTP mode, accumulates status/headers from `ResponseStart` and builds
/// an [`OutboundResponse`] directly — no intermediate mpsc channel for the
/// common fixed-response case.
///
/// In WebSocket mode, forwards events via mpsc (same as before).
#[pyclass(module = "apx._core", freelist = 64)]
pub struct AsgiSend {
    inner: SendInner,
}

impl std::fmt::Debug for AsgiSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            SendInner::Http { .. } => f.debug_struct("AsgiSend::Http").finish_non_exhaustive(),
            SendInner::Ws { .. } => f.debug_struct("AsgiSend::Ws").finish_non_exhaustive(),
        }
    }
}

impl AsgiSend {
    /// Create an HTTP-mode sender backed by a oneshot response channel.
    pub fn http(
        response_tx: oneshot::Sender<Result<OutboundResponse, AppError>>,
        disconnect_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            inner: SendInner::Http {
                status: None,
                headers: None,
                response_tx: Some(response_tx),
                disconnect_tx: Some(disconnect_tx),
                stream_tx: None,
            },
        }
    }

    /// Create a WebSocket-mode sender backed by an mpsc channel.
    pub fn new(tx: mpsc::Sender<AsgiEvent>) -> Self {
        Self {
            inner: SendInner::Ws { tx },
        }
    }
}

#[pymethods]
impl AsgiSend {
    /// Python: `await send({"type": "http.response.start", ...})`
    fn __call__<'py>(
        &mut self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let t0 = super::bench_trace_enabled().then(std::time::Instant::now);
        let parsed = parse_asgi_send_event(&event)?;
        if let Some(t0) = t0 {
            tracing::info!(
                target: "bench_trace",
                phase = "parse_asgi_send_event",
                elapsed_us = t0.elapsed().as_micros(),
            );
        }

        match &mut self.inner {
            SendInner::Http {
                status,
                headers,
                response_tx,
                disconnect_tx,
                stream_tx,
            } => Self::handle_http(
                py,
                parsed,
                status,
                headers,
                response_tx,
                disconnect_tx,
                stream_tx,
            ),
            SendInner::Ws { tx } => Self::handle_ws(py, parsed, tx),
        }
    }
}

impl AsgiSend {
    /// Handle an event in HTTP mode.
    fn handle_http<'py>(
        py: Python<'py>,
        event: AsgiEvent,
        status: &mut Option<u16>,
        headers: &mut Option<HeaderMap>,
        response_tx: &mut Option<oneshot::Sender<Result<OutboundResponse, AppError>>>,
        disconnect_tx: &mut Option<oneshot::Sender<()>>,
        stream_tx: &mut Option<mpsc::Sender<AsgiEvent>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match event {
            AsgiEvent::ResponseStart {
                status: s,
                headers: h,
            } => {
                *status = Some(s);
                *headers = Some(h);
                Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any())
            }
            AsgiEvent::ResponseBody { body, more_body } if stream_tx.is_none() => {
                // First body chunk.
                let Some(raw_status) = status.take() else {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "ASGI protocol error: body before response start",
                    ));
                };
                let resp_headers = headers.take().unwrap_or_default();
                let http_status = http::StatusCode::from_u16(raw_status)
                    .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);

                if more_body {
                    // Streaming — create mpsc for remaining chunks.
                    let (stx, srx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
                    let dtx = disconnect_tx.take();
                    let stream = super::streaming::AsgiBodyStream::new(srx, Some(body), dtx);
                    if let Some(tx) = response_tx.take() {
                        let _ = tx.send(Ok(OutboundResponse {
                            status: http_status,
                            headers: resp_headers,
                            body: ResponseBody::Stream(Box::pin(stream)),
                        }));
                    }
                    *stream_tx = Some(stx);
                } else {
                    // Fixed response — send via oneshot, drop disconnect_tx.
                    let _ = disconnect_tx.take();
                    if let Some(tx) = response_tx.take() {
                        let _ = tx.send(Ok(OutboundResponse {
                            status: http_status,
                            headers: resp_headers,
                            body: ResponseBody::Fixed(body),
                        }));
                    }
                }
                Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any())
            }
            AsgiEvent::ResponseBody { body, more_body } => {
                // Subsequent streaming chunk — forward to mpsc.
                let Some(tx) = stream_tx.as_ref() else {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "ASGI protocol error: body after stream closed",
                    ));
                };
                match tx.try_send(AsgiEvent::ResponseBody { body, more_body }) {
                    Ok(()) => {
                        if !more_body {
                            *stream_tx = None;
                        }
                        Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any())
                    }
                    Err(mpsc::error::TrySendError::Full(event)) => {
                        let (future, resolve_tx) =
                            crate::scheduler::primitives::Future::with_channel();
                        let py_future = Py::new(py, future)?;
                        let tx = tx.clone();
                        let drop_stream = !more_body;
                        crate::scheduler::with_tokio_handle(|handle| {
                            handle.spawn(async move {
                                let _ = tx.send(event).await;
                                Python::attach(|py| {
                                    let _ = resolve_tx.send(py.None());
                                });
                            });
                        })
                        .ok_or_else(|| {
                            pyo3::exceptions::PyRuntimeError::new_err(
                                "no tokio runtime for backpressure send",
                            )
                        })?;
                        if drop_stream {
                            *stream_tx = None;
                        }
                        Ok(py_future.into_bound(py).into_any())
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        *stream_tx = None;
                        Err(pyo3::exceptions::PyRuntimeError::new_err(
                            "stream channel closed",
                        ))
                    }
                }
            }
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "unexpected event type in HTTP mode",
            )),
        }
    }

    /// Handle an event in WebSocket mode (unchanged logic).
    fn handle_ws<'py>(
        py: Python<'py>,
        event: AsgiEvent,
        tx: &mpsc::Sender<AsgiEvent>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match tx.try_send(event) {
            Ok(()) => Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any()),
            Err(mpsc::error::TrySendError::Full(event)) => {
                let (future, resolve_tx) = crate::scheduler::primitives::Future::with_channel();
                let py_future = Py::new(py, future)?;
                let tx = tx.clone();
                crate::scheduler::with_tokio_handle(|handle| {
                    handle.spawn(async move {
                        let _ = tx.send(event).await;
                        Python::attach(|py| {
                            let _ = resolve_tx.send(py.None());
                        });
                    });
                })
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for backpressure send",
                    )
                })?;
                Ok(py_future.into_bound(py).into_any())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(
                pyo3::exceptions::PyRuntimeError::new_err("response channel closed"),
            ),
        }
    }
}

// ── WebSocket incoming events ────────────────────────────────────────────

/// Incoming WebSocket event from the client (axum WS → Python handler).
#[derive(Debug)]
pub enum WsIncomingEvent {
    /// `websocket.connect` — initial connection event.
    Connect,
    /// `websocket.receive` — client sent a text or binary frame.
    Receive {
        /// Text frame payload.
        text: Option<String>,
        /// Binary frame payload.
        bytes: Option<Vec<u8>>,
    },
    /// `websocket.disconnect` — client disconnected.
    Disconnect {
        /// WebSocket close code (default 1000).
        code: u16,
    },
}

/// ASGI `receive` callable for WebSocket connections.
///
/// Returns ASGI dicts for `websocket.connect`, `websocket.receive`,
/// and `websocket.disconnect` events by reading from a channel fed
/// by the axum WebSocket frame forwarder.
#[pyclass(module = "apx._core")]
pub struct AsgiWsReceive {
    rx: Arc<Mutex<mpsc::Receiver<WsIncomingEvent>>>,
}

impl std::fmt::Debug for AsgiWsReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiWsReceive").finish_non_exhaustive()
    }
}

impl AsgiWsReceive {
    /// Create a new WebSocket receive callable.
    pub fn new(rx: mpsc::Receiver<WsIncomingEvent>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
        }
    }
}

#[pymethods]
impl AsgiWsReceive {
    /// Python: `event = await receive()`
    fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = rx.lock().await;
            let event = guard.recv().await;
            Python::attach(|py| build_ws_receive_event(py, event))
        })
    }
}

/// Build an ASGI WebSocket receive event dict.
fn build_ws_receive_event(py: Python<'_>, event: Option<WsIncomingEvent>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    let type_key = pyo3::intern!(py, "type");
    match event {
        Some(WsIncomingEvent::Connect) => {
            dict.set_item(type_key, pyo3::intern!(py, "websocket.connect"))?;
        }
        Some(WsIncomingEvent::Receive { text, bytes }) => {
            dict.set_item(type_key, pyo3::intern!(py, "websocket.receive"))?;
            if let Some(t) = text {
                dict.set_item(pyo3::intern!(py, "text"), t)?;
            }
            if let Some(b) = bytes {
                dict.set_item(pyo3::intern!(py, "bytes"), PyBytes::new(py, &b))?;
            }
        }
        Some(WsIncomingEvent::Disconnect { code }) => {
            dict.set_item(type_key, pyo3::intern!(py, "websocket.disconnect"))?;
            dict.set_item(pyo3::intern!(py, "code"), code)?;
        }
        None => {
            dict.set_item(type_key, pyo3::intern!(py, "websocket.disconnect"))?;
            dict.set_item(pyo3::intern!(py, "code"), 1000u16)?;
        }
    }
    Ok(dict.into_any().unbind())
}

// ── Parse helpers ────────────────────────────────────────────────────────

/// Parse an ASGI send event dict into a typed [`AsgiEvent`].
///
/// Compares the `"type"` value against interned Python strings directly,
/// avoiding a Rust `String` allocation on every call. Only the error path
/// (unsupported event type) extracts the string for the error message.
fn parse_asgi_send_event(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let type_obj = event
        .get_item(pyo3::intern!(py, "type"))?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?;

    if type_obj.eq(pyo3::intern!(py, "http.response.start"))? {
        parse_response_start(event)
    } else if type_obj.eq(pyo3::intern!(py, "http.response.body"))? {
        parse_response_body(event)
    } else if type_obj.eq(pyo3::intern!(py, "websocket.accept"))? {
        parse_ws_accept(event)
    } else if type_obj.eq(pyo3::intern!(py, "websocket.send"))? {
        parse_ws_send(event)
    } else if type_obj.eq(pyo3::intern!(py, "websocket.close"))? {
        parse_ws_close(event)
    } else {
        let event_type: String = type_obj.extract()?;
        Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unsupported ASGI event type: {event_type}"
        )))
    }
}

/// Parse `http.response.start` — extract status and build `HeaderMap` directly.
///
/// Builds the `HeaderMap` from `PyBytes` references without intermediate
/// `Vec<u8>` allocations. Standard header names (content-type, etc.) are
/// recognized as constants with zero allocation.
fn parse_response_start(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let status: u16 = event
        .get_item(pyo3::intern!(py, "status"))?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("status"))?
        .extract()?;
    let headers = parse_header_map(event)?;
    Ok(AsgiEvent::ResponseStart { status, headers })
}

/// Parse `http.response.body` — extract body bytes and more_body flag.
fn parse_response_body(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let body = extract_body_bytes(event)?;
    let more_body: bool = event
        .get_item(pyo3::intern!(py, "more_body"))?
        .map(|b| b.extract())
        .transpose()?
        .unwrap_or(false);
    Ok(AsgiEvent::ResponseBody { body, more_body })
}

/// Extract body bytes from an ASGI event dict, preferring zero-copy via `PyBytes`.
fn extract_body_bytes(event: &Bound<'_, PyDict>) -> PyResult<Bytes> {
    let py = event.py();
    let Some(obj) = event.get_item(pyo3::intern!(py, "body"))? else {
        return Ok(Bytes::new());
    };
    match obj.cast::<PyBytes>() {
        Ok(py_bytes) => Ok(Bytes::copy_from_slice(py_bytes.as_bytes())),
        Err(_) => Ok(Bytes::from(obj.extract::<Vec<u8>>()?)),
    }
}

/// Parse `websocket.accept` — extract optional subprotocol and headers.
fn parse_ws_accept(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let subprotocol: Option<String> = event
        .get_item(pyo3::intern!(py, "subprotocol"))?
        .and_then(|v| v.extract().ok());
    let headers = extract_header_list(event)?;
    Ok(AsgiEvent::WsAccept {
        subprotocol,
        headers,
    })
}

/// Parse `websocket.send` — extract text or binary payload.
fn parse_ws_send(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let text: Option<String> = event
        .get_item(pyo3::intern!(py, "text"))?
        .and_then(|v| v.extract().ok());
    let bytes: Option<Vec<u8>> = event
        .get_item(pyo3::intern!(py, "bytes"))?
        .and_then(|v| v.extract().ok());
    Ok(AsgiEvent::WsSend { text, bytes })
}

/// Parse `websocket.close` — extract close code.
fn parse_ws_close(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let code: u16 = event
        .get_item(pyo3::intern!(py, "code"))?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(1000);
    Ok(AsgiEvent::WsClose { code })
}

/// Build an `http::HeaderMap` directly from an ASGI headers list.
///
/// Reads `[(b"name", b"value"), ...]` from the Python dict and constructs
/// `HeaderName`/`HeaderValue` directly from `PyBytes::as_bytes()` borrows,
/// eliminating intermediate `Vec<u8>` allocations per header.
fn parse_header_map(event: &Bound<'_, PyDict>) -> PyResult<HeaderMap> {
    let py = event.py();
    let Some(list) = event.get_item(pyo3::intern!(py, "headers"))? else {
        return Ok(HeaderMap::new());
    };
    let iter = list.try_iter()?;
    let size_hint = iter.size_hint().0;
    let mut headers = HeaderMap::with_capacity(size_hint);
    for item in iter {
        let tuple = item?;
        let name = header_name_from_py(&tuple.get_item(0)?)?;
        let value = header_value_from_py(&tuple.get_item(1)?)?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// Build a `HeaderName` from a Python bytes-like object.
fn header_name_from_py(obj: &Bound<'_, PyAny>) -> PyResult<HeaderName> {
    let bytes = match obj.cast::<PyBytes>() {
        Ok(py_bytes) => py_bytes.as_bytes(),
        Err(_) => return header_name_from_extracted(obj),
    };
    HeaderName::from_bytes(bytes)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid header name: {e}")))
}

/// Fallback: extract bytes then parse header name.
fn header_name_from_extracted(obj: &Bound<'_, PyAny>) -> PyResult<HeaderName> {
    let bytes: Vec<u8> = obj.extract()?;
    HeaderName::from_bytes(&bytes)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid header name: {e}")))
}

/// Build a `HeaderValue` from a Python bytes-like object.
fn header_value_from_py(obj: &Bound<'_, PyAny>) -> PyResult<HeaderValue> {
    let bytes = match obj.cast::<PyBytes>() {
        Ok(py_bytes) => py_bytes.as_bytes(),
        Err(_) => return header_value_from_extracted(obj),
    };
    HeaderValue::from_bytes(bytes)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid header value: {e}")))
}

/// Fallback: extract bytes then parse header value.
fn header_value_from_extracted(obj: &Bound<'_, PyAny>) -> PyResult<HeaderValue> {
    let bytes: Vec<u8> = obj.extract()?;
    HeaderValue::from_bytes(&bytes)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid header value: {e}")))
}

/// Extract raw byte pairs from an ASGI headers list (for WebSocket events).
fn extract_header_list(event: &Bound<'_, PyDict>) -> PyResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let Some(list) = event.get_item(pyo3::intern!(event.py(), "headers"))? else {
        return Ok(Vec::new());
    };
    list.try_iter()?
        .map(|item| {
            let tuple = item?;
            let name = extract_bytes_field(&tuple.get_item(0)?)?;
            let value = extract_bytes_field(&tuple.get_item(1)?)?;
            Ok((name, value))
        })
        .collect()
}

/// Extract a `Vec<u8>` from a Python object, preferring direct `PyBytes` borrow.
fn extract_bytes_field(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    match obj.cast::<PyBytes>() {
        Ok(py_bytes) => Ok(py_bytes.as_bytes().to_vec()),
        Err(_) => obj.extract::<Vec<u8>>(),
    }
}

// ── build_http_scope ─────────────────────────────────────────────────────

// CPython internal: create a dict pre-sized for `minused` keys.
// Stable across CPython 3.8-3.13. Not exposed by pyo3-ffi (marked private),
// so we declare it manually.
#[expect(unsafe_code, reason = "CPython FFI declaration for dict pre-sizing")]
unsafe extern "C" {
    fn _PyDict_NewPresized(minused: pyo3::ffi::Py_ssize_t) -> *mut pyo3::ffi::PyObject;
}

/// Create a `PyDict` with pre-allocated capacity.
///
/// Avoids internal rehashing for dicts with a known number of keys.
#[expect(unsafe_code, reason = "CPython FFI for dict pre-sizing")]
fn new_presized_dict(py: Python<'_>, capacity: isize) -> Bound<'_, PyDict> {
    let ptr = unsafe { _PyDict_NewPresized(capacity) };
    if ptr.is_null() {
        return PyDict::new(py);
    }
    unsafe { Bound::from_owned_ptr(py, ptr).cast_into_unchecked() }
}

/// Expected key count for HTTP scope dicts (type, asgi, http_version, method,
/// path, raw_path, query_string, headers, server, client, scheme, root_path,
/// state, path_params + optional app, router).
const HTTP_SCOPE_KEY_COUNT: isize = 14;

/// Expected key count for WebSocket scope dicts.
const WS_SCOPE_KEY_COUNT: isize = 12;

/// Construct an ASGI HTTP scope dict from an [`InboundRequest`].
///
/// This is the bridge between the transport-neutral request abstraction
/// and the ASGI protocol. It must never receive hyper/axum types directly.
///
/// When `fastapi_app` is provided, `scope["app"]` and `scope["router"]` are
/// set so that FastAPI/Starlette routing and dependency injection work.
pub fn build_http_scope(
    py: Python<'_>,
    request: &InboundRequest,
    fastapi_app: Option<&Py<PyAny>>,
    interns: &ScopeInterns,
) -> PyResult<Py<PyDict>> {
    let dict = new_presized_dict(py, HTTP_SCOPE_KEY_COUNT);
    set_scope_metadata(py, &dict, interns)?;
    set_scope_request_fields(py, &dict, request, interns)?;
    set_scope_headers(py, &dict, request, interns)?;
    set_scope_addresses(py, &dict, request, interns)?;
    set_scope_path_params(py, &dict, request, interns)?;
    dict.set_item(interns.keys.state.bind(py), PyDict::new(py))?;
    if let Some(app) = fastapi_app {
        dict.set_item(interns.keys.app.bind(py), app.bind(py))?;
        dict.set_item(
            interns.keys.router.bind(py),
            app.bind(py).getattr(c"router")?,
        )?;
    }
    Ok(dict.unbind())
}

/// Construct an ASGI WebSocket scope dict from an [`InboundRequest`].
///
/// Similar to [`build_http_scope`] but sets `type: "websocket"` and `scheme: "ws"`.
/// No body-related fields.
pub fn build_ws_scope(
    py: Python<'_>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<Py<PyDict>> {
    let dict = new_presized_dict(py, WS_SCOPE_KEY_COUNT);
    set_ws_scope_metadata(py, &dict, interns)?;
    set_ws_scope_request_fields(py, &dict, request, interns)?;
    set_scope_headers(py, &dict, request, interns)?;
    set_scope_addresses(py, &dict, request, interns)?;
    set_scope_path_params(py, &dict, request, interns)?;
    dict.set_item(interns.keys.state.bind(py), PyDict::new(py))?;
    Ok(dict.unbind())
}

/// Set ASGI WebSocket scope metadata fields.
fn set_ws_scope_metadata(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.r#type.bind(py),
        interns.vals.type_websocket.bind(py),
    )?;
    dict.set_item(interns.keys.asgi.bind(py), interns.vals.asgi_dict.bind(py))?;
    dict.set_item(
        interns.keys.scheme.bind(py),
        interns.vals.scheme_ws.bind(py),
    )?;
    dict.set_item(
        interns.keys.root_path.bind(py),
        interns.vals.root_path_empty.bind(py),
    )?;
    Ok(())
}

/// Set WebSocket request-specific scope fields.
fn set_ws_scope_request_fields(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.http_version.bind(py),
        request.protocol.as_asgi_version(),
    )?;
    dict.set_item(interns.keys.path.bind(py), percent_decode(&request.path))?;
    dict.set_item(
        interns.keys.raw_path.bind(py),
        PyBytes::new(py, request.path.as_bytes()),
    )?;
    dict.set_item(
        interns.keys.query_string.bind(py),
        PyBytes::new(py, &request.query_string),
    )?;
    Ok(())
}

/// Set ASGI scope metadata fields: type, asgi, http_version, scheme, root_path.
fn set_scope_metadata(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.r#type.bind(py),
        interns.vals.type_http.bind(py),
    )?;
    dict.set_item(interns.keys.asgi.bind(py), interns.vals.asgi_dict.bind(py))?;
    dict.set_item(
        interns.keys.scheme.bind(py),
        interns.vals.scheme_http.bind(py),
    )?;
    dict.set_item(
        interns.keys.root_path.bind(py),
        interns.vals.root_path_empty.bind(py),
    )?;
    Ok(())
}

/// Set request-specific scope fields: http_version, method, path, raw_path, query_string.
fn set_scope_request_fields(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.http_version.bind(py),
        request.protocol.as_asgi_version(),
    )?;
    dict.set_item(interns.keys.method.bind(py), request.method.as_str())?;
    // ASGI spec: "path" is the decoded URL path, "raw_path" is the raw bytes.
    dict.set_item(interns.keys.path.bind(py), percent_decode(&request.path))?;
    dict.set_item(
        interns.keys.raw_path.bind(py),
        PyBytes::new(py, request.path.as_bytes()),
    )?;
    dict.set_item(
        interns.keys.query_string.bind(py),
        PyBytes::new(py, &request.query_string),
    )?;
    Ok(())
}

/// Set ASGI headers as a list of `(bytes, bytes)` tuples.
///
/// Uses cached `PyBytes` for common header names (cache hit = zero allocation)
/// and constructs the list from a presized `Vec` (zero list resizes).
fn set_scope_headers(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    let mut pairs: Vec<Bound<'_, PyAny>> = Vec::with_capacity(request.headers.len());
    for (name, value) in &request.headers {
        let n = interns
            .headers
            .get(py, name)
            .unwrap_or_else(|| PyBytes::new(py, name.as_str().as_bytes()));
        let v = PyBytes::new(py, value.as_bytes());
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        pairs.push(pair.into_any());
    }
    let headers_list = PyList::new(py, &pairs)?;
    dict.set_item(interns.keys.headers.bind(py), headers_list)?;
    Ok(())
}

/// Set server and client address tuples in scope.
fn set_scope_addresses(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.server.bind(py),
        (
            request.server_addr.ip().to_string(),
            request.server_addr.port(),
        ),
    )?;
    match request.client_addr {
        Some(addr) => {
            dict.set_item(
                interns.keys.client.bind(py),
                (addr.ip().to_string(), addr.port()),
            )?;
        }
        None => dict.set_item(interns.keys.client.bind(py), py.None())?,
    }
    Ok(())
}

/// Set path_params dict in scope (Starlette reads `scope["path_params"]`).
///
/// Values are URL-decoded because axum's `RawPathParams` provides percent-encoded
/// strings, but Starlette/FastAPI expects decoded values (matching what Starlette's
/// own router would produce).
fn set_scope_path_params(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    let pp = PyDict::new(py);
    for (k, v) in &request.path_params {
        pp.set_item(k.as_str(), percent_decode(v.as_str()))?;
    }
    dict.set_item(interns.keys.path_params.bind(py), pp)?;
    Ok(())
}

/// Decode percent-encoded UTF-8 strings (e.g., `hello%20world` → `hello world`).
///
/// Returns the original string borrowed if no percent sequences are present,
/// avoiding a heap allocation on the common path.
pub(super) fn percent_decode(input: &str) -> Cow<'_, str> {
    if !input.contains('%') {
        return Cow::Borrowed(input);
    }
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.as_bytes().iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(h), Some(l)) = (hi, lo) {
                if let (Some(hv), Some(lv)) = (hex_val(h), hex_val(l)) {
                    bytes.push(hv << 4 | lv);
                    continue;
                }
                // Invalid hex — emit literally
                bytes.extend_from_slice(&[b'%', h, l]);
            } else {
                // Truncated — emit literally
                bytes.push(b'%');
                if let Some(h) = hi {
                    bytes.push(h);
                }
            }
        } else {
            bytes.push(b);
        }
    }
    Cow::Owned(String::from_utf8(bytes).unwrap_or_else(|_| input.to_owned()))
}

/// Convert an ASCII hex digit to its 4-bit value.
const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::transport::types::{BodyStream, ProtocolVersion, TransportKind};
    use crate::with_py;
    use http::header::HeaderMap;
    use std::net::SocketAddr;

    // ── Pure Rust tests ──────────────────────────────────────────────────

    #[test]
    fn asgi_event_debug_response_start() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "text/plain".parse().unwrap());
        let event = AsgiEvent::ResponseStart {
            status: 200,
            headers: h,
        };
        let dbg = format!("{event:?}");
        assert!(dbg.contains("ResponseStart"));
        assert!(dbg.contains("200"));
    }

    #[test]
    fn asgi_event_debug_response_body() {
        let event = AsgiEvent::ResponseBody {
            body: Bytes::from("hello"),
            more_body: false,
        };
        let dbg = format!("{event:?}");
        assert!(dbg.contains("ResponseBody"));
    }

    #[test]
    fn asgi_send_debug_http() {
        let (tx, _rx) = oneshot::channel();
        let (dtx, _drx) = oneshot::channel();
        let send = AsgiSend::http(tx, dtx);
        let dbg = format!("{send:?}");
        assert!(dbg.contains("AsgiSend::Http"));
    }

    #[test]
    fn asgi_send_debug_ws() {
        let (tx, _rx) = mpsc::channel(1);
        let send = AsgiSend::new(tx);
        let dbg = format!("{send:?}");
        assert!(dbg.contains("AsgiSend::Ws"));
    }

    // ── Helper ───────────────────────────────────────────────────────────

    fn make_inbound_request(
        method: http::Method,
        path: &str,
        query: &[u8],
        headers: HeaderMap,
        path_params: Vec<(String, String)>,
        client_addr: Option<SocketAddr>,
    ) -> InboundRequest {
        InboundRequest::new(
            method,
            path.to_owned(),
            Bytes::copy_from_slice(query),
            headers,
            BodyStream::Empty,
            ProtocolVersion::Http11,
            TransportKind::Tcp,
            client_addr,
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            path_params,
            http::Extensions::new(),
        )
    }

    // ── build_http_scope tests (require Python) ──────────────────────────

    #[test]
    fn scope_basic_fields() {
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            assert_eq!(
                scope
                    .get_item("type")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "http"
            );
            assert_eq!(
                scope
                    .get_item("method")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "GET"
            );
            assert_eq!(
                scope
                    .get_item("path")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "/"
            );
            assert_eq!(
                scope
                    .get_item("scheme")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "http"
            );
            assert_eq!(
                scope
                    .get_item("root_path")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                ""
            );
            // asgi version
            let asgi = scope.get_item("asgi").unwrap().unwrap();
            assert_eq!(
                asgi.get_item("version")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "3.0"
            );
            assert_eq!(
                asgi.get_item("spec_version")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "2.4"
            );
        });
    }

    #[test]
    fn scope_protocol_versions() {
        with_py(|py| {
            for (version, expected) in [
                (ProtocolVersion::Http10, "1.0"),
                (ProtocolVersion::Http11, "1.1"),
                (ProtocolVersion::H2, "2"),
            ] {
                let req = InboundRequest::new(
                    http::Method::GET,
                    "/".to_owned(),
                    Bytes::new(),
                    HeaderMap::new(),
                    BodyStream::Empty,
                    version,
                    TransportKind::Tcp,
                    None,
                    SocketAddr::from(([127, 0, 0, 1], 8080)),
                    Vec::new(),
                    http::Extensions::new(),
                );
                let interns = ScopeInterns::new(py);
                let scope = build_http_scope(py, &req, None, &interns).unwrap();
                let scope = scope.bind(py);
                let http_version: String = scope
                    .get_item("http_version")
                    .unwrap()
                    .unwrap()
                    .extract()
                    .unwrap();
                assert_eq!(http_version, expected, "version {version:?}");
            }
        });
    }

    #[test]
    fn scope_with_query_string() {
        let req = make_inbound_request(
            http::Method::GET,
            "/search",
            b"q=hello&page=1",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let qs: Vec<u8> = scope
                .get_item("query_string")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(qs, b"q=hello&page=1");
        });
    }

    #[test]
    fn scope_with_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-custom", "value".parse().unwrap());
        let req = make_inbound_request(http::Method::POST, "/api", b"", headers, Vec::new(), None);
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let headers_list = scope.get_item("headers").unwrap().unwrap();
            let len = headers_list.len().unwrap();
            assert_eq!(len, 2);
        });
    }

    #[test]
    fn scope_with_path_params() {
        let req = make_inbound_request(
            http::Method::GET,
            "/items/42",
            b"",
            HeaderMap::new(),
            vec![("item_id".to_owned(), "42".to_owned())],
            None,
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let pp = scope.get_item("path_params").unwrap().unwrap();
            let val: String = pp.get_item("item_id").unwrap().extract().unwrap();
            assert_eq!(val, "42");
        });
    }

    #[test]
    fn scope_with_client_addr() {
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            Some(SocketAddr::from(([192, 168, 1, 100], 12345))),
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let client = scope.get_item("client").unwrap().unwrap();
            let host: String = client.get_item(0).unwrap().extract().unwrap();
            let port: u16 = client.get_item(1).unwrap().extract().unwrap();
            assert_eq!(host, "192.168.1.100");
            assert_eq!(port, 12345);
        });
    }

    #[test]
    fn scope_no_client() {
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let client = scope.get_item("client").unwrap().unwrap();
            assert!(client.is_none());
        });
    }

    #[test]
    fn scope_server_addr() {
        let req = make_inbound_request(
            http::Method::GET,
            "/",
            b"",
            HeaderMap::new(),
            Vec::new(),
            None,
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_http_scope(py, &req, None, &interns).unwrap();
            let scope = scope.bind(py);
            let server = scope.get_item("server").unwrap().unwrap();
            let host: String = server.get_item(0).unwrap().extract().unwrap();
            let port: u16 = server.get_item(1).unwrap().extract().unwrap();
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 8080);
        });
    }

    #[test]
    fn receive_disconnect_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item(
                pyo3::intern!(py, "type"),
                pyo3::intern!(py, "http.disconnect"),
            )
            .unwrap();

            let event_type: String = dict.get_item("type").unwrap().unwrap().extract().unwrap();
            assert_eq!(event_type, "http.disconnect");
        });
    }

    // ── AsgiSend parse + channel tests ───────────────────────────────────

    #[test]
    fn parse_response_start_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.start").unwrap();
            dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            let h = PyTuple::new(
                py,
                [
                    PyBytes::new(py, b"content-type").into_any(),
                    PyBytes::new(py, b"text/plain").into_any(),
                ],
            )
            .unwrap();
            headers.append(h).unwrap();
            dict.set_item("headers", headers).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseStart { status, headers } => {
                    assert_eq!(status, 200);
                    assert_eq!(headers.len(), 1);
                    assert_eq!(headers.get("content-type").unwrap(), "text/plain");
                }
                other => panic!("expected ResponseStart, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_response_body_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.body").unwrap();
            dict.set_item("body", PyBytes::new(py, b"hello")).unwrap();
            dict.set_item("more_body", false).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseBody { body, more_body } => {
                    assert_eq!(body.as_ref(), b"hello");
                    assert!(!more_body);
                }
                other => panic!("expected ResponseBody, got {other:?}"),
            }
        });
    }

    #[tokio::test]
    async fn asgi_send_http_fixed_response() {
        let (response_tx, response_rx) = oneshot::channel();
        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut send = AsgiSend::http(response_tx, disconnect_tx);

        with_py(|py| {
            // Send ResponseStart.
            let start_dict = PyDict::new(py);
            start_dict.set_item("type", "http.response.start").unwrap();
            start_dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            start_dict.set_item("headers", headers).unwrap();
            send.__call__(py, start_dict.clone()).unwrap();

            // Send ResponseBody (more_body=false).
            let body_dict = PyDict::new(py);
            body_dict.set_item("type", "http.response.body").unwrap();
            body_dict
                .set_item("body", PyBytes::new(py, b"hello"))
                .unwrap();
            body_dict.set_item("more_body", false).unwrap();
            send.__call__(py, body_dict.clone()).unwrap();
        });

        let resp = response_rx.await.unwrap().unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
        match resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), b"hello"),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[tokio::test]
    async fn asgi_send_http_streaming_response() {
        let (response_tx, response_rx) = oneshot::channel();
        let (disconnect_tx, _disconnect_rx) = oneshot::channel();
        let mut send = AsgiSend::http(response_tx, disconnect_tx);

        with_py(|py| {
            // Send ResponseStart.
            let start_dict = PyDict::new(py);
            start_dict.set_item("type", "http.response.start").unwrap();
            start_dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            start_dict.set_item("headers", headers).unwrap();
            send.__call__(py, start_dict.clone()).unwrap();

            // Send ResponseBody (more_body=true → streaming).
            let body_dict = PyDict::new(py);
            body_dict.set_item("type", "http.response.body").unwrap();
            body_dict
                .set_item("body", PyBytes::new(py, b"chunk1"))
                .unwrap();
            body_dict.set_item("more_body", true).unwrap();
            send.__call__(py, body_dict.clone()).unwrap();
        });

        let resp = response_rx.await.unwrap().unwrap();
        assert_eq!(resp.status, http::StatusCode::OK);
        match resp.body {
            ResponseBody::Stream(mut stream) => {
                use futures_core::Stream;
                let waker = futures_util::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);
                match std::pin::Pin::new(&mut stream).poll_next(&mut cx) {
                    std::task::Poll::Ready(Some(Ok(chunk))) => {
                        assert_eq!(chunk.as_ref(), b"chunk1");
                    }
                    other => panic!("expected Ready(Some(Ok(...))), got {other:?}"),
                }
            }
            ResponseBody::Fixed(_) => panic!("expected Stream body"),
        }
    }

    #[test]
    fn send_unknown_event_type() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.unknown").unwrap();
            let result = parse_asgi_send_event(&dict);
            assert!(result.is_err());
            let err_str = result.unwrap_err().to_string();
            assert!(err_str.contains("unsupported ASGI event type"));
        });
    }

    #[test]
    fn send_missing_type_key() {
        with_py(|py| {
            let dict = PyDict::new(py);
            let result = parse_asgi_send_event(&dict);
            assert!(result.is_err());
        });
    }

    // ── WebSocket event parse tests ─────────────────────────────────────

    #[test]
    fn parse_ws_accept_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.accept").unwrap();
            dict.set_item("subprotocol", "graphql-ws").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsAccept {
                    subprotocol,
                    headers,
                } => {
                    assert_eq!(subprotocol.as_deref(), Some("graphql-ws"));
                    assert!(headers.is_empty());
                }
                other => panic!("expected WsAccept, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_send_text_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.send").unwrap();
            dict.set_item("text", "hello").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsSend { text, bytes } => {
                    assert_eq!(text.as_deref(), Some("hello"));
                    assert!(bytes.is_none());
                }
                other => panic!("expected WsSend, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_send_binary_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.send").unwrap();
            dict.set_item("bytes", PyBytes::new(py, b"\x01\x02\x03"))
                .unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsSend { text, bytes } => {
                    assert!(text.is_none());
                    assert_eq!(bytes.as_deref(), Some(b"\x01\x02\x03".as_slice()));
                }
                other => panic!("expected WsSend, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_close_event() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.close").unwrap();
            dict.set_item("code", 1001u16).unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsClose { code } => {
                    assert_eq!(code, 1001);
                }
                other => panic!("expected WsClose, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_close_default_code() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.close").unwrap();

            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsClose { code } => {
                    assert_eq!(code, 1000);
                }
                other => panic!("expected WsClose, got {other:?}"),
            }
        });
    }

    #[test]
    fn ws_incoming_event_debug() {
        let connect = WsIncomingEvent::Connect;
        assert!(format!("{connect:?}").contains("Connect"));

        let recv = WsIncomingEvent::Receive {
            text: Some("hello".to_owned()),
            bytes: None,
        };
        assert!(format!("{recv:?}").contains("Receive"));

        let disc = WsIncomingEvent::Disconnect { code: 1000 };
        assert!(format!("{disc:?}").contains("Disconnect"));
    }

    #[test]
    fn asgi_ws_receive_debug() {
        let (_tx, rx) = mpsc::channel(1);
        let recv = AsgiWsReceive::new(rx);
        let dbg = format!("{recv:?}");
        assert!(dbg.contains("AsgiWsReceive"));
    }

    // ── build_ws_scope tests ────────────────────────────────────────────

    #[test]
    fn build_ws_scope_basic() {
        let req = make_inbound_request(
            http::Method::GET,
            "/ws",
            b"token=abc",
            HeaderMap::new(),
            vec![("room".to_owned(), "main".to_owned())],
            Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
        );
        with_py(|py| {
            let interns = ScopeInterns::new(py);
            let scope = build_ws_scope(py, &req, &interns).unwrap();
            let scope = scope.bind(py);

            let scope_type: String = scope.get_item("type").unwrap().unwrap().extract().unwrap();
            assert_eq!(scope_type, "websocket");

            let scheme: String = scope
                .get_item("scheme")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(scheme, "ws");

            let path: String = scope.get_item("path").unwrap().unwrap().extract().unwrap();
            assert_eq!(path, "/ws");

            let qs: Vec<u8> = scope
                .get_item("query_string")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(qs, b"token=abc");

            // path params
            let pp = scope.get_item("path_params").unwrap().unwrap();
            let room: String = pp.get_item("room").unwrap().extract().unwrap();
            assert_eq!(room, "main");

            // no 'method' key (WS scope doesn't have method)
            assert!(scope.get_item("method").unwrap().is_none());
        });
    }

    // ── build_ws_receive_event tests ─────────────────────────────────────

    #[test]
    fn build_ws_receive_event_connect() {
        with_py(|py| {
            let result = build_ws_receive_event(py, Some(WsIncomingEvent::Connect)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.connect");
        });
    }

    #[test]
    fn build_ws_receive_event_receive_text() {
        with_py(|py| {
            let event = WsIncomingEvent::Receive {
                text: Some("hello".to_owned()),
                bytes: None,
            };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.receive");
            let text: String = dict.get_item("text").unwrap().extract().unwrap();
            assert_eq!(text, "hello");
        });
    }

    #[test]
    fn build_ws_receive_event_receive_bytes() {
        with_py(|py| {
            let event = WsIncomingEvent::Receive {
                text: None,
                bytes: Some(vec![0x01, 0x02, 0x03]),
            };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.receive");
            let bytes: Vec<u8> = dict.get_item("bytes").unwrap().extract().unwrap();
            assert_eq!(bytes, vec![0x01, 0x02, 0x03]);
        });
    }

    #[test]
    fn build_ws_receive_event_disconnect_with_code() {
        with_py(|py| {
            let event = WsIncomingEvent::Disconnect { code: 1001 };
            let result = build_ws_receive_event(py, Some(event)).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.disconnect");
            let code: u16 = dict.get_item("code").unwrap().extract().unwrap();
            assert_eq!(code, 1001);
        });
    }

    #[test]
    fn build_ws_receive_event_channel_closed() {
        with_py(|py| {
            let result = build_ws_receive_event(py, None).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "websocket.disconnect");
            let code: u16 = dict.get_item("code").unwrap().extract().unwrap();
            assert_eq!(code, 1000);
        });
    }

    // ── parse edge case tests ────────────────────────────────────────────

    #[test]
    fn parse_response_body_missing_body_key() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "http.response.body").unwrap();
            // No "body" key, no "more_body" key — defaults to empty body, more_body=false
            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::ResponseBody { body, more_body } => {
                    assert!(body.is_empty());
                    assert!(!more_body);
                }
                other => panic!("expected ResponseBody, got {other:?}"),
            }
        });
    }

    #[test]
    fn parse_ws_accept_no_subprotocol() {
        with_py(|py| {
            let dict = PyDict::new(py);
            dict.set_item("type", "websocket.accept").unwrap();
            let event = parse_asgi_send_event(&dict).unwrap();
            match event {
                AsgiEvent::WsAccept {
                    subprotocol,
                    headers,
                } => {
                    assert!(subprotocol.is_none());
                    assert!(headers.is_empty());
                }
                other => panic!("expected WsAccept, got {other:?}"),
            }
        });
    }
}
