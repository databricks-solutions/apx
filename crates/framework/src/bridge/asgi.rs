//! ASGI protocol primitives backed by Rust.
//!
//! Provides `AsgiReceive`, `AsgiSend` (Python callables), and `build_http_scope`
//! for constructing ASGI HTTP scope dicts from [`InboundRequest`].
//!
//! These types enable Starlette's `Request`, `StreamingResponse`, and `WebSocket`
//! to work unmodified against a Rust-backed ASGI server.

use crate::transport::types::InboundRequest;
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyDictMethods, PyList, PyString, PyTuple};
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// ASGI protocol version string.
const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
const ASGI_SPEC_VERSION: &str = "2.3";

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

impl ScopeInterns {
    /// Create all interned strings. Call once at worker startup with GIL held.
    pub(crate) fn new(py: Python<'_>) -> Self {
        let s = |v: &str| PyString::new(py, v).unbind();

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
        }
    }
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
        /// Response headers as raw byte pairs.
        headers: Vec<(Vec<u8>, Vec<u8>)>,
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
#[pyclass(module = "apx._core")]
pub struct AsgiReceive {
    body: std::sync::Mutex<Option<Bytes>>,
}

impl std::fmt::Debug for AsgiReceive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiReceive").finish_non_exhaustive()
    }
}

impl AsgiReceive {
    /// Create for an HTTP request with a known body.
    pub fn http(body: Bytes) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(body)),
        }
    }

    /// Create for an HTTP request with no body (GET, HEAD, DELETE).
    pub fn empty() -> Self {
        Self {
            body: std::sync::Mutex::new(Some(Bytes::new())),
        }
    }

    /// Alias for `http` — used by the sync dispatch path.
    pub fn immediate(body: Bytes) -> Self {
        Self::http(body)
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

        match taken {
            Some(bytes) => {
                let event = build_receive_event(py, Some(bytes))?;
                Py::new(py, ResolvedAwaitableWithValue { value: Some(event) })
                    .map(|obj| obj.into_bound(py).into_any())
            }
            None => {
                // Body already consumed — pend forever for disconnect listener.
                pyo3_async_runtimes::tokio::future_into_py(py, async {
                    std::future::pending::<PyResult<Py<PyAny>>>().await
                })
            }
        }
    }
}

/// Build the ASGI receive event dict.
fn build_receive_event(py: Python<'_>, body: Option<Bytes>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    match body {
        Some(bytes) => {
            dict.set_item("type", "http.request")?;
            dict.set_item("body", PyBytes::new(py, &bytes))?;
            dict.set_item("more_body", false)?;
        }
        None => {
            dict.set_item("type", "http.disconnect")?;
        }
    }
    Ok(dict.into_any().unbind())
}

// ── ResolvedAwaitable ─────────────────────────────────────────────────────

/// Zero-overhead Python awaitable that completes immediately.
///
/// Used by buffered `AsgiSend` to avoid `pyo3_async_runtimes::future_into_py`
/// and its tokio task overhead. Implements the Python iterator protocol
/// so `await resolved_awaitable` returns `None` with no scheduling.
#[pyclass(module = "apx._core")]
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
/// Used by `AsgiReceive::immediate` to return the receive dict without
/// `future_into_py` (which requires a tokio runtime, unavailable on
/// `spawn_blocking` threads).
#[pyclass(module = "apx._core")]
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

/// Send backend: channel for streaming, buffer for buffered responses.
enum SendBackend {
    /// Streaming: events flow through an mpsc channel to a concurrent reader.
    Channel(mpsc::Sender<AsgiEvent>),
    /// Buffered: events accumulate in a shared Vec, read after coroutine completion.
    Buffer(Arc<std::sync::Mutex<Vec<AsgiEvent>>>),
}

/// Shared buffer for buffered ASGI response collection.
///
/// Created before scheduling the coroutine. After the coroutine completes,
/// the Tokio side calls [`take`](AsgiEventBuffer::take) to drain the events.
#[derive(Clone)]
pub struct AsgiEventBuffer(Arc<std::sync::Mutex<Vec<AsgiEvent>>>);

impl AsgiEventBuffer {
    /// Create a new empty buffer.
    pub fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(Vec::with_capacity(2))))
    }

    /// Drain all accumulated events.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned (handler panicked mid-send).
    #[expect(clippy::unwrap_used, reason = "poisoned mutex indicates handler panic")]
    pub fn take(&self) -> Vec<AsgiEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl std::fmt::Debug for AsgiEventBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiEventBuffer").finish_non_exhaustive()
    }
}

/// ASGI `send` callable backed by Rust.
///
/// Supports two modes:
/// - **Channel** (streaming): pushes events through an mpsc channel for
///   concurrent response collection.
/// - **Buffer** (buffered): accumulates events in a shared `Vec` for
///   synchronous draining after coroutine completion. Avoids mpsc channel
///   allocation and `future_into_py` overhead.
#[pyclass(module = "apx._core")]
pub struct AsgiSend {
    backend: SendBackend,
}

impl std::fmt::Debug for AsgiSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsgiSend").finish_non_exhaustive()
    }
}

impl AsgiSend {
    /// Create a channel-backed sender for streaming responses.
    pub fn channel(tx: mpsc::Sender<AsgiEvent>) -> Self {
        Self {
            backend: SendBackend::Channel(tx),
        }
    }

    /// Create a buffer-backed sender for buffered responses.
    pub fn buffered(buffer: &AsgiEventBuffer) -> Self {
        Self {
            backend: SendBackend::Buffer(Arc::clone(&buffer.0)),
        }
    }
}

#[pymethods]
impl AsgiSend {
    /// Python: `await send({"type": "http.response.start", ...})`
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let parsed = parse_asgi_send_event(&event)?;
        match &self.backend {
            SendBackend::Channel(tx) => {
                let tx = tx.clone();
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    tx.send(parsed).await.map_err(|_| {
                        pyo3::exceptions::PyRuntimeError::new_err("response channel closed")
                    })?;
                    Python::attach(|py| Ok(py.None()))
                })
            }
            SendBackend::Buffer(buf) => {
                buf.lock()
                    .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("send buffer poisoned"))?
                    .push(parsed);
                Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any())
            }
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
    match event {
        Some(WsIncomingEvent::Connect) => {
            dict.set_item("type", "websocket.connect")?;
        }
        Some(WsIncomingEvent::Receive { text, bytes }) => {
            dict.set_item("type", "websocket.receive")?;
            if let Some(t) = text {
                dict.set_item("text", t)?;
            }
            if let Some(b) = bytes {
                dict.set_item("bytes", PyBytes::new(py, &b))?;
            }
        }
        Some(WsIncomingEvent::Disconnect { code }) => {
            dict.set_item("type", "websocket.disconnect")?;
            dict.set_item("code", code)?;
        }
        None => {
            dict.set_item("type", "websocket.disconnect")?;
            dict.set_item("code", 1000u16)?;
        }
    }
    Ok(dict.into_any().unbind())
}

// ── Parse helpers ────────────────────────────────────────────────────────

/// Parse an ASGI send event dict into a typed [`AsgiEvent`].
fn parse_asgi_send_event(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let event_type: String = event
        .get_item("type")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?
        .extract()?;

    match event_type.as_str() {
        "http.response.start" => parse_response_start(event),
        "http.response.body" => parse_response_body(event),
        "websocket.accept" => parse_ws_accept(event),
        "websocket.send" => parse_ws_send(event),
        "websocket.close" => parse_ws_close(event),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unsupported ASGI event type: {other}"
        ))),
    }
}

/// Parse `http.response.start` — extract status and headers.
fn parse_response_start(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let status: u16 = event
        .get_item("status")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("status"))?
        .extract()?;
    let headers = extract_header_list(event)?;
    Ok(AsgiEvent::ResponseStart { status, headers })
}

/// Parse `http.response.body` — extract body bytes and more_body flag.
fn parse_response_body(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let body = extract_body_bytes(event)?;
    let more_body: bool = event
        .get_item("more_body")?
        .map(|b| b.extract())
        .transpose()?
        .unwrap_or(false);
    Ok(AsgiEvent::ResponseBody { body, more_body })
}

/// Extract body bytes from an ASGI event dict, preferring zero-copy via `PyBytes`.
fn extract_body_bytes(event: &Bound<'_, PyDict>) -> PyResult<Bytes> {
    let Some(obj) = event.get_item("body")? else {
        return Ok(Bytes::new());
    };
    match obj.cast::<PyBytes>() {
        Ok(py_bytes) => Ok(Bytes::copy_from_slice(py_bytes.as_bytes())),
        Err(_) => Ok(Bytes::from(obj.extract::<Vec<u8>>()?)),
    }
}

/// Parse `websocket.accept` — extract optional subprotocol and headers.
fn parse_ws_accept(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let subprotocol: Option<String> = event
        .get_item("subprotocol")?
        .and_then(|v| v.extract().ok());
    let headers = extract_header_list(event)?;
    Ok(AsgiEvent::WsAccept {
        subprotocol,
        headers,
    })
}

/// Parse `websocket.send` — extract text or binary payload.
fn parse_ws_send(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let text: Option<String> = event.get_item("text")?.and_then(|v| v.extract().ok());
    let bytes: Option<Vec<u8>> = event.get_item("bytes")?.and_then(|v| v.extract().ok());
    Ok(AsgiEvent::WsSend { text, bytes })
}

/// Parse `websocket.close` — extract close code.
fn parse_ws_close(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let code: u16 = event
        .get_item("code")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(1000);
    Ok(AsgiEvent::WsClose { code })
}

/// Parse ASGI headers list: `[(b"name", b"value"), ...]`.
///
/// Uses `PyBytes::as_bytes()` to borrow directly from Python objects,
/// avoiding pyo3's generic extraction overhead per header pair.
fn extract_header_list(event: &Bound<'_, PyDict>) -> PyResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let Some(list) = event.get_item("headers")? else {
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
    let dict = PyDict::new(py);
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
    let dict = PyDict::new(py);
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
fn set_scope_headers(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    let headers_list = PyList::empty(py);
    for (name, value) in &request.headers {
        let n = PyBytes::new(py, name.as_str().as_bytes());
        let v = PyBytes::new(py, value.as_bytes());
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        headers_list.append(pair)?;
    }
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
fn percent_decode(input: &str) -> Cow<'_, str> {
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

// ── ASGI Lifespan Protocol ───────────────────────────────────────────────

// Gated behind cfg(test) — only the integration test harness uses lifespan.
// Remove the gate when production `apx serve` runs lifespan startup.
#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test-only lifespan helpers use unwrap for infallible PyDict operations"
)]
pub mod lifespan {
    use super::*;
    use crate::error::AppError;
    use crate::event_loop::EventLoopHandle;
    use tokio::sync::oneshot;

    /// Startup-complete signal sender.
    type StartupTx = Arc<std::sync::Mutex<Option<oneshot::Sender<Result<(), String>>>>>;

    /// RAII guard for a running ASGI lifespan. Sends `lifespan.shutdown` on drop.
    pub struct LifespanGuard {
        /// Send `lifespan.shutdown` when ready to stop.
        shutdown_tx: Option<mpsc::Sender<Py<PyAny>>>,
        /// Join handle for the background lifespan task (the ASGI app coroutine).
        _task: Option<tokio::task::JoinHandle<()>>,
    }

    impl Drop for LifespanGuard {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                // Build shutdown event and send it (best-effort on drop).
                // Use try_send (non-blocking) since drop may run inside a tokio runtime.
                Python::attach(|py| {
                    let event = build_lifespan_event(py, "lifespan.shutdown");
                    let _ = tx.try_send(event);
                });
            }
        }
    }

    /// ASGI `receive` callable for the lifespan protocol.
    ///
    /// Yields events from a channel: first `lifespan.startup`, then
    /// `lifespan.shutdown` (sent by [`LifespanGuard`] on drop).
    #[pyclass(module = "apx._core")]
    struct AsgiLifespanReceive {
        rx: Arc<Mutex<mpsc::Receiver<Py<PyAny>>>>,
    }

    #[pymethods]
    impl AsgiLifespanReceive {
        fn __call__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
            let rx = Arc::clone(&self.rx);
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let mut guard = rx.lock().await;
                match guard.recv().await {
                    Some(event) => Ok(event),
                    None => {
                        // Channel closed — block forever to prevent the ASGI app
                        // from returning prematurely.
                        std::future::pending::<PyResult<Py<PyAny>>>().await
                    }
                }
            })
        }
    }

    /// ASGI `send` callable for the lifespan protocol.
    ///
    /// Captures `lifespan.startup.complete` and `lifespan.shutdown.complete`.
    #[pyclass(module = "apx._core")]
    struct AsgiLifespanSend {
        startup_complete_tx: StartupTx,
    }

    #[pymethods]
    impl AsgiLifespanSend {
        fn __call__<'py>(
            &self,
            py: Python<'py>,
            event: Bound<'py, PyDict>,
        ) -> PyResult<Bound<'py, PyAny>> {
            let event_type: String = event
                .get_item("type")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("type"))?
                .extract()?;

            match event_type.as_str() {
                "lifespan.startup.complete" => {
                    let maybe_tx = self.startup_complete_tx.lock().unwrap().take();
                    if let Some(tx) = maybe_tx {
                        let _ = tx.send(Ok(()));
                    }
                }
                "lifespan.startup.failed" => {
                    let message: String = event
                        .get_item("message")?
                        .map(|v| v.extract())
                        .transpose()?
                        .unwrap_or_default();
                    let maybe_tx = self.startup_complete_tx.lock().unwrap().take();
                    if let Some(tx) = maybe_tx {
                        let _ = tx.send(Err(message));
                    }
                }
                "lifespan.shutdown.complete" | "lifespan.shutdown.failed" => {
                    // Nothing to do — the ASGI app will return on its own.
                }
                other => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "unexpected lifespan event: {other}"
                    )));
                }
            }

            // Return an awaitable None.
            pyo3_async_runtimes::tokio::future_into_py(py, async {
                Python::attach(|py| Ok(py.None()))
            })
        }
    }

    /// Build a lifespan event dict: `{"type": <event_type>}`.
    fn build_lifespan_event(py: Python<'_>, event_type: &str) -> Py<PyAny> {
        let dict = PyDict::new(py);
        dict.set_item("type", event_type).unwrap();
        dict.into_any().unbind()
    }

    /// Build the ASGI lifespan scope dict.
    fn build_lifespan_scope(py: Python<'_>) -> Py<PyDict> {
        let dict = PyDict::new(py);
        dict.set_item("type", "lifespan").unwrap();
        let asgi = PyDict::new(py);
        asgi.set_item("version", ASGI_VERSION).unwrap();
        asgi.set_item("spec_version", ASGI_SPEC_VERSION).unwrap();
        dict.set_item("asgi", asgi).unwrap();
        dict.unbind()
    }

    /// Run the ASGI lifespan startup protocol against the app.
    ///
    /// Returns a [`LifespanGuard`] whose drop sends `lifespan.shutdown`.
    /// The ASGI app coroutine runs in the background on the event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if startup fails or the event loop can't drive the coroutine.
    pub async fn run_lifespan_startup(
        app: &Py<PyAny>,
        loop_handle: &EventLoopHandle,
    ) -> Result<LifespanGuard, AppError> {
        // Channel for receive events (startup, then shutdown on drop).
        let (receive_tx, receive_rx) = mpsc::channel::<Py<PyAny>>(2);

        // Oneshot for startup-complete signal.
        let (startup_tx, startup_rx) = oneshot::channel::<Result<(), String>>();

        // Build ASGI objects and call app(scope, receive, send).
        let coro = Python::attach(|py| -> Result<Py<PyAny>, AppError> {
            let scope = build_lifespan_scope(py);

            let receive = AsgiLifespanReceive {
                rx: Arc::new(Mutex::new(receive_rx)),
            };
            let send = AsgiLifespanSend {
                startup_complete_tx: Arc::new(std::sync::Mutex::new(Some(startup_tx))),
            };

            let receive_obj = Py::new(py, receive)
                .map_err(|e| AppError::Internal(format!("wrap lifespan receive: {e}")))?;
            let send_obj = Py::new(py, send)
                .map_err(|e| AppError::Internal(format!("wrap lifespan send: {e}")))?;

            app.call(py, (scope, receive_obj, send_obj), None)
                .map_err(|e| AppError::Internal(format!("lifespan call: {e}")))
        })?;

        // Send the startup event through the receive channel.
        let startup_event = Python::attach(|py| build_lifespan_event(py, "lifespan.startup"));
        receive_tx.send(startup_event).await.map_err(|_| {
            AppError::Internal("lifespan receive channel closed before startup".to_owned())
        })?;

        // Spawn the ASGI app coroutine on the event loop (don't await — it runs
        // until shutdown).
        let lh = loop_handle.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = lh.drive_coroutine(coro).await {
                tracing::warn!(error = %e, "lifespan coroutine error");
            }
        });

        // Wait for the app to signal startup complete.
        let result = startup_rx.await.map_err(|_| {
            AppError::Internal("lifespan startup: app never sent startup.complete".to_owned())
        })?;

        if let Err(message) = result {
            return Err(AppError::Internal(format!(
                "lifespan startup failed: {message}"
            )));
        }

        Ok(LifespanGuard {
            shutdown_tx: Some(receive_tx),
            _task: Some(task),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
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
        let event = AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"text/plain".to_vec())],
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
    fn asgi_receive_debug() {
        let recv = AsgiReceive::empty();
        let dbg = format!("{recv:?}");
        assert!(dbg.contains("AsgiReceive"));
    }

    #[test]
    fn asgi_send_debug() {
        let (tx, _rx) = mpsc::channel(1);
        let send = AsgiSend::channel(tx);
        let dbg = format!("{send:?}");
        assert!(dbg.contains("AsgiSend"));
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
                "2.3"
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

    // ── AsgiReceive logic tests ─────────────────────────────────────────

    #[tokio::test]
    async fn receive_http_body_then_disconnect() {
        let body = Arc::new(Mutex::new(Some(Bytes::from("hello"))));

        // First call: http.request with body
        let taken = body.lock().await.take();
        with_py(|py| {
            let result = build_receive_event(py, taken).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.request");
            let body_bytes: Vec<u8> = dict.get_item("body").unwrap().extract().unwrap();
            assert_eq!(body_bytes, b"hello");
            let more: bool = dict.get_item("more_body").unwrap().extract().unwrap();
            assert!(!more);
        });

        // Second call: http.disconnect
        let taken = body.lock().await.take();
        with_py(|py| {
            let result = build_receive_event(py, taken).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.disconnect");
        });
    }

    #[test]
    fn receive_empty_body() {
        with_py(|py| {
            let result = build_receive_event(py, Some(Bytes::new())).unwrap();
            let dict = result.bind(py);
            let event_type: String = dict.get_item("type").unwrap().extract().unwrap();
            assert_eq!(event_type, "http.request");
            let body_bytes: Vec<u8> = dict.get_item("body").unwrap().extract().unwrap();
            assert!(body_bytes.is_empty());
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
                    assert_eq!(headers[0].0, b"content-type");
                    assert_eq!(headers[0].1, b"text/plain");
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
    async fn send_event_through_channel() {
        let (tx, mut rx) = mpsc::channel(4);
        let event = AsgiEvent::ResponseStart {
            status: 200,
            headers: vec![(b"content-type".to_vec(), b"text/plain".to_vec())],
        };
        tx.send(event).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert!(matches!(
            received,
            AsgiEvent::ResponseStart { status: 200, .. }
        ));
    }

    #[tokio::test]
    async fn send_channel_closed_returns_error() {
        let (tx, rx) = mpsc::channel::<AsgiEvent>(1);
        drop(rx);
        let event = AsgiEvent::ResponseBody {
            body: Bytes::from("x"),
            more_body: false,
        };
        assert!(tx.send(event).await.is_err());
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
