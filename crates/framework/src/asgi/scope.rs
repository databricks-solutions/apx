//! ASGI protocol primitives backed by Rust.
//!
//! Provides `AsgiReceive`, `AsgiSend` (Python callables), `scope_from_template`,
//! and `build_ws_scope` for constructing ASGI scope dicts from [`InboundRequest`].
//!
//! These types enable Starlette's `Request`, `StreamingResponse`, and `WebSocket`
//! to work unmodified against a Rust-backed ASGI server.

use crate::protocol::http::error::AppError;
use crate::transport::types::{InboundRequest, OutboundResponse, ProtocolVersion, ResponseBody};
use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::{PyBytes, PyDict, PyDictMethods, PyList, PyString, PyTuple};
use std::borrow::Cow;
use std::net::SocketAddr;
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

crate::opaque_debug!(ScopeInterns);

/// Pre-interned Python strings for ASGI scope construction.
///
/// Created once at worker startup, shared across all requests via `AppState`.
/// Eliminates ~25 transient `PyString` allocations per request.
pub struct ScopeInterns {
    // ── Scope dict keys ──
    /// Fixed keys used in every ASGI scope dict.
    pub(crate) keys: ScopeKeys,
    // ── Scope dict fixed values ──
    /// Fixed values (type strings, version strings, empty root_path).
    pub(crate) vals: ScopeValues,
    // ── Header name cache ──
    /// Cached `PyBytes` for common HTTP header names.
    pub(crate) headers: HeaderInterns,
    // ── Per-worker address cache ──
    /// Pre-built `(host_str, port)` tuple for the server address.
    pub(crate) server_tuple: Py<PyTuple>,
    // ── HTTP version interns ──
    /// Cached `PyString` for HTTP protocol versions.
    pub(crate) versions: VersionInterns,
    // ── Scope template ──
    /// Pre-built HTTP scope dict with fixed fields. `dict.copy()` per request.
    pub(crate) scope_template: Py<PyDict>,
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

/// Pre-interned `PyString` for common HTTP methods.
///
/// Uses pointer comparison on `http::Method` constants for O(1) lookup.
/// Pre-interned `PyString` for HTTP protocol versions ("1.0", "1.1", "2").
pub struct VersionInterns {
    http10: Py<PyString>,
    http11: Py<PyString>,
    h2: Py<PyString>,
}

impl VersionInterns {
    /// Create cached `PyString` for protocol versions. Call once at worker startup.
    fn new(py: Python<'_>) -> Self {
        Self {
            http10: PyString::intern(py, "1.0").clone().unbind(),
            http11: PyString::intern(py, "1.1").clone().unbind(),
            h2: PyString::intern(py, "2").clone().unbind(),
        }
    }

    /// Get the interned `PyString` for a protocol version.
    pub fn get<'py>(&self, py: Python<'py>, version: ProtocolVersion) -> Bound<'py, PyString> {
        match version {
            ProtocolVersion::Http10 => self.http10.bind(py).clone(),
            ProtocolVersion::Http11 => self.http11.bind(py).clone(),
            ProtocolVersion::H2 => self.h2.bind(py).clone(),
        }
    }
}

// ── SendCache ────────────────────────────────────────────────────────────

/// Cached Python objects for the ASGI send path.
///
/// Separate from `ScopeInterns` (smallest possible scope): scope-building
/// code never touches these, and send code never touches scope interns.
pub struct SendCache {
    /// Singleton `ResolvedAwaitable` — stateless, reused via `clone_ref`.
    pub(crate) resolved: Py<ResolvedAwaitable>,
}

crate::opaque_debug!(SendCache);

impl SendCache {
    /// Create the send cache. Call once at worker startup with GIL held.
    pub fn new(py: Python<'_>) -> PyResult<Self> {
        Ok(Self {
            resolved: Py::new(py, ResolvedAwaitable)?,
        })
    }
}

impl ScopeInterns {
    /// Create all interned strings and cached objects.
    ///
    /// Call once at worker startup with GIL held.
    /// Accepts `server_addr` to pre-build the server address tuple.
    #[expect(
        clippy::expect_used,
        reason = "infallible Python conversions at startup"
    )]
    pub(crate) fn new(py: Python<'_>, server_addr: SocketAddr) -> Self {
        let s = |v: &str| PyString::intern(py, v).clone().unbind();

        let asgi_dict = PyDict::new(py);
        let _ = asgi_dict.set_item(s("version").bind(py), s(ASGI_VERSION).bind(py));
        let _ = asgi_dict.set_item(s("spec_version").bind(py), s(ASGI_SPEC_VERSION).bind(py));

        let server_tuple = PyTuple::new(
            py,
            [
                server_addr
                    .ip()
                    .to_string()
                    .into_pyobject(py)
                    .expect("ip string")
                    .into_any(),
                server_addr
                    .port()
                    .into_pyobject(py)
                    .expect("port int")
                    .into_any(),
            ],
        )
        .expect("server tuple")
        .unbind();

        let keys = ScopeKeys {
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
        };
        let vals = ScopeValues {
            type_http: s("http"),
            type_websocket: s("websocket"),
            scheme_http: s(DEFAULT_SCHEME),
            scheme_ws: s(WS_SCHEME),
            root_path_empty: s(""),
            asgi_dict: asgi_dict.unbind(),
        };
        let versions = VersionInterns::new(py);

        // Build scope template with fixed HTTP fields pre-populated.
        let scope_template = {
            let tpl = PyDict::new(py);
            let _ = tpl.set_item(keys.r#type.bind(py), vals.type_http.bind(py));
            let _ = tpl.set_item(keys.asgi.bind(py), vals.asgi_dict.bind(py));
            let _ = tpl.set_item(keys.scheme.bind(py), vals.scheme_http.bind(py));
            let _ = tpl.set_item(keys.root_path.bind(py), vals.root_path_empty.bind(py));
            let _ = tpl.set_item(keys.http_version.bind(py), versions.http11.bind(py));
            let _ = tpl.set_item(keys.server.bind(py), server_tuple.bind(py));
            tpl.unbind()
        };

        Self {
            keys,
            vals,
            headers: HeaderInterns::new(py),
            server_tuple,
            versions,
            scope_template,
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
        /// Binary frame payload (zero-copy from Python via `PyBackedBytes`).
        bytes: Option<Bytes>,
    },
    /// `websocket.close` — server closes the connection.
    WsClose {
        /// WebSocket close code (default 1000).
        code: u16,
    },
}

// ── AsgiReceive ──────────────────────────────────────────────────────────

/// Build the `receive` template dict: `{"type": "http.request", "body": b"", "more_body": False}`.
///
/// Created once per worker, cloned per-request via `PyDict::copy`. This is
/// faster than building 3 dict keys from scratch each time.
pub fn build_receive_template(py: Python<'_>) -> PyResult<Py<PyDict>> {
    let d = PyDict::new(py);
    d.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))?;
    d.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, b""))?;
    d.set_item(pyo3::intern!(py, "more_body"), false)?;
    Ok(d.unbind())
}

/// ASGI `receive` callable backed by Rust.
///
/// For HTTP: first call returns `http.request` with the pre-buffered body
/// synchronously (via `ResolvedAwaitableWithValue`, no tokio task overhead).
/// Subsequent calls pend forever via `future_into_py` + `pending()`,
/// preventing Starlette's `listen_for_disconnect` from prematurely firing.
#[pyclass(module = "apx._core", freelist = 64)]
pub struct AsgiReceive {
    body: std::sync::Mutex<Option<Bytes>>,
    disconnect_rx: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
    receive_template: Py<PyDict>,
}

crate::opaque_debug!(AsgiReceive);

impl AsgiReceive {
    /// Create for an HTTP request with a known body.
    pub fn http(
        body: Bytes,
        disconnect_rx: oneshot::Receiver<()>,
        receive_template: Py<PyDict>,
    ) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(body)),
            disconnect_rx: std::sync::Mutex::new(Some(disconnect_rx)),
            receive_template,
        }
    }

    /// Create for an HTTP request with no body (GET, HEAD, DELETE).
    pub fn empty(disconnect_rx: oneshot::Receiver<()>, receive_template: Py<PyDict>) -> Self {
        Self {
            body: std::sync::Mutex::new(Some(Bytes::new())),
            disconnect_rx: std::sync::Mutex::new(Some(disconnect_rx)),
            receive_template,
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
            let event = crate::telemetry::timed!(
                crate::telemetry::dispatch_metrics::record_receive_build,
                {
                    let event = self.receive_template.bind(py).copy()?;
                    event.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, &bytes))?;
                    event
                }
            );
            let event = event.unbind().into_any();
            Py::new(py, ResolvedAwaitableWithValue { value: Some(event) })
                .map(|obj| obj.into_bound(py).into_any())
        } else {
            let maybe_disconnect = self
                .disconnect_rx
                .lock()
                .map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err("disconnect mutex poisoned")
                })?
                .take();
            let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("no tokio runtime for disconnect watch")
            })?;
            let _guard = handle.enter();
            if let Some(disconnect_rx) = maybe_disconnect {
                let disconnect_type = pyo3::intern!(py, "http.disconnect").clone().unbind();
                let type_key = pyo3::intern!(py, "type").clone().unbind();
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    let _ = disconnect_rx.await;
                    Python::attach(|py| -> PyResult<Py<PyAny>> {
                        let event = PyDict::new(py);
                        event.set_item(&type_key, &disconnect_type)?;
                        Ok(event.unbind().into_any())
                    })
                })
            } else {
                pyo3_async_runtimes::tokio::future_into_py(
                    py,
                    std::future::pending::<PyResult<Py<PyAny>>>(),
                )
            }
        }
    }
}

// ── ResolvedAwaitable ─────────────────────────────────────────────────────

/// Zero-overhead Python awaitable that completes immediately.
///
/// Used by buffered `AsgiSend` to avoid `pyo3_async_runtimes::future_into_py`
/// and its tokio task overhead. Implements the Python iterator protocol
/// so `await resolved_awaitable` returns `None` with no scheduling.
#[expect(
    clippy::redundant_pub_crate,
    reason = "visible to sibling modules in asgi/"
)]
#[pyclass(module = "apx._core", freelist = 128)]
pub(crate) struct ResolvedAwaitable;

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
/// Used by `AsgiReceive` and `SlotReceive` to return the receive dict
/// without `future_into_py` (which requires a tokio runtime, unavailable
/// on `spawn_blocking` threads).
#[expect(
    clippy::redundant_pub_crate,
    reason = "visible to sibling modules in asgi/"
)]
#[pyclass(module = "apx._core", freelist = 64)]
pub(crate) struct ResolvedAwaitableWithValue {
    value: Option<Py<PyAny>>,
}

impl ResolvedAwaitableWithValue {
    /// Create a new resolved awaitable that will return `value`.
    pub(crate) fn new(value: Py<PyAny>) -> Self {
        Self { value: Some(value) }
    }
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
///
/// Must be at least as large as the drive step budget (128) so that a
/// streaming handler producing many small chunks never blocks during a
/// single drive cycle. Backpressure still engages for very large
/// responses — the driver suspends and the drain task resumes once hyper
/// drains the channel.
const STREAM_CHANNEL_CAPACITY: usize = 256;

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
    resolved: Option<Py<ResolvedAwaitable>>,
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
        send_cache: &SendCache,
        py: Python<'_>,
    ) -> Self {
        Self {
            inner: SendInner::Http {
                status: None,
                headers: None,
                response_tx: Some(response_tx),
                disconnect_tx: Some(disconnect_tx),
                stream_tx: None,
            },
            resolved: Some(send_cache.resolved.clone_ref(py)),
        }
    }

    /// Create a WebSocket-mode sender backed by an mpsc channel.
    pub fn new(tx: mpsc::Sender<AsgiEvent>) -> Self {
        Self {
            inner: SendInner::Ws { tx },
            resolved: None,
        }
    }
}

#[pymethods]
impl AsgiSend {
    /// Forward an unhandled app exception through the response channel as a 500.
    ///
    /// Called by the `_guarded` wrapper when the ASGI app raises an
    /// `Exception`. Without this, `response_tx` drops silently and
    /// `response_rx` gets `RecvError`.
    fn send_error(&mut self, traceback: String) {
        if let SendInner::Http { response_tx, .. } = &mut self.inner
            && let Some(tx) = response_tx.take()
        {
            let _ = tx.send(Err(AppError::Internal(traceback)));
        }
    }

    /// Python: `await send({"type": "http.response.start", ...})`
    fn __call__<'py>(
        &mut self,
        py: Python<'py>,
        event: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let parsed = crate::telemetry::timed!(
            crate::telemetry::dispatch_metrics::record_send_parse,
            parse_asgi_send_event(&event)?
        );

        let resolved = self.resolved.as_ref();
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
                resolved,
            ),
            SendInner::Ws { tx } => Self::handle_ws(py, parsed, tx, resolved),
        }
    }
}

impl AsgiSend {
    /// Return a `ResolvedAwaitable` from the cached singleton or a fresh allocation.
    fn resolved_awaitable<'py>(
        resolved: Option<&Py<ResolvedAwaitable>>,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(cached) = resolved {
            Ok(cached.clone_ref(py).into_bound(py).into_any())
        } else {
            Py::new(py, ResolvedAwaitable).map(|obj| obj.into_bound(py).into_any())
        }
    }

    /// Handle an event in HTTP mode.
    #[expect(
        clippy::too_many_arguments,
        reason = "mutable refs to send state fields"
    )]
    fn handle_http<'py>(
        py: Python<'py>,
        event: AsgiEvent,
        status: &mut Option<u16>,
        headers: &mut Option<HeaderMap>,
        response_tx: &mut Option<oneshot::Sender<Result<OutboundResponse, AppError>>>,
        disconnect_tx: &mut Option<oneshot::Sender<()>>,
        stream_tx: &mut Option<mpsc::Sender<AsgiEvent>>,
        resolved: Option<&Py<ResolvedAwaitable>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match event {
            AsgiEvent::ResponseStart {
                status: s,
                headers: h,
            } => {
                tracing::trace!(name: "apx.asgi.send_response_start", status = s, "asgi_send: response_start");
                *status = Some(s);
                *headers = Some(h);
                Self::resolved_awaitable(resolved, py)
            }
            AsgiEvent::ResponseBody { body, more_body } if stream_tx.is_none() => {
                Self::handle_first_body(
                    py,
                    body,
                    more_body,
                    status,
                    headers,
                    response_tx,
                    disconnect_tx,
                    stream_tx,
                    resolved,
                )
            }
            AsgiEvent::ResponseBody { body, more_body } => {
                Self::handle_stream_body(py, body, more_body, stream_tx, resolved)
            }
            _ => Err(pyo3::exceptions::PyRuntimeError::new_err(
                "unexpected event type in HTTP mode",
            )),
        }
    }

    /// First `http.response.body` — decide streaming vs fixed and send the response.
    #[expect(
        clippy::too_many_arguments,
        reason = "mutable refs to send state fields"
    )]
    fn handle_first_body<'py>(
        py: Python<'py>,
        body: Bytes,
        more_body: bool,
        status: &mut Option<u16>,
        headers: &mut Option<HeaderMap>,
        response_tx: &mut Option<oneshot::Sender<Result<OutboundResponse, AppError>>>,
        disconnect_tx: &mut Option<oneshot::Sender<()>>,
        stream_tx: &mut Option<mpsc::Sender<AsgiEvent>>,
        resolved: Option<&Py<ResolvedAwaitable>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Some(raw_status) = status.take() else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "ASGI protocol error: body before response start",
            ));
        };
        let resp_headers = headers.take().unwrap_or_default();
        let http_status = http::StatusCode::from_u16(raw_status)
            .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);
        let server_route = None;

        if more_body {
            let (stx, srx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
            let dtx = disconnect_tx.take();
            let body_len = body.len();
            let stream = super::streaming::AsgiBodyStream::new(srx, Some(body), dtx);
            if let Some(tx) = response_tx.take() {
                let _ = tx.send(Ok(OutboundResponse {
                    status: http_status,
                    headers: resp_headers,
                    body: ResponseBody::Stream(Box::pin(stream)),
                    server_route,
                }));
            }
            *stream_tx = Some(stx);
            tracing::trace!(name: "apx.asgi.send_first_body_chunk", body_len, "asgi_send: first body chunk (streaming started)");
        } else {
            let _ = disconnect_tx.take();
            let body_len = body.len();
            if let Some(tx) = response_tx.take() {
                let _ = tx.send(Ok(OutboundResponse {
                    status: http_status,
                    headers: resp_headers,
                    body: ResponseBody::Fixed(body),
                    server_route,
                }));
            }
            tracing::trace!(name: "apx.asgi.send_fixed_body", body_len, "asgi_send: fixed body (complete)");
        }
        Self::resolved_awaitable(resolved, py)
    }

    /// Subsequent `http.response.body` — push to the streaming channel with backpressure.
    fn handle_stream_body<'py>(
        py: Python<'py>,
        body: Bytes,
        more_body: bool,
        stream_tx: &mut Option<mpsc::Sender<AsgiEvent>>,
        resolved: Option<&Py<ResolvedAwaitable>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Some(tx) = stream_tx.as_ref() else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "ASGI protocol error: body after stream closed",
            ));
        };
        let body_len = body.len();
        match tx.try_send(AsgiEvent::ResponseBody { body, more_body }) {
            Ok(()) => {
                tracing::trace!(
                    name: "apx.asgi.send_stream_chunk",
                    body_len,
                    more_body,
                    "asgi_send: stream chunk sent (no backpressure)"
                );
                if !more_body {
                    *stream_tx = None;
                }
                Self::resolved_awaitable(resolved, py)
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                tracing::trace!(
                    name: "apx.asgi.send_stream_backpressure",
                    body_len,
                    more_body,
                    "asgi_send: stream chunk BACKPRESSURE (channel full)"
                );
                let tx = tx.clone();
                let drop_stream = !more_body;
                let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for backpressure send",
                    )
                })?;
                let _guard = handle.enter();
                if drop_stream {
                    *stream_tx = None;
                }
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    let _ = tx.send(event).await;
                    tracing::trace!(name: "apx.asgi.send_backpressure_resolved", "asgi_send: backpressure resolved");
                    Ok(Python::attach(|py| py.None()))
                })
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::trace!(name: "apx.asgi.send_stream_channel_closed", "asgi_send: stream channel CLOSED");
                *stream_tx = None;
                Err(pyo3::exceptions::PyRuntimeError::new_err(
                    "stream channel closed",
                ))
            }
        }
    }

    /// Handle an event in WebSocket mode (unchanged logic).
    fn handle_ws<'py>(
        py: Python<'py>,
        event: AsgiEvent,
        tx: &mpsc::Sender<AsgiEvent>,
        resolved: Option<&Py<ResolvedAwaitable>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match tx.try_send(event) {
            Ok(()) => Self::resolved_awaitable(resolved, py),
            Err(mpsc::error::TrySendError::Full(event)) => {
                let tx = tx.clone();
                let handle = crate::io::with_tokio_handle(|h| h.clone()).ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "no tokio runtime for backpressure send",
                    )
                })?;
                let _guard = handle.enter();
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    let _ = tx.send(event).await;
                    Ok(Python::attach(|py| py.None()))
                })
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
        /// Binary frame payload (zero-copy from tungstenite `Bytes`).
        bytes: Option<Bytes>,
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

crate::opaque_debug!(AsgiWsReceive);

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

/// Extract body bytes from an ASGI event dict via zero-copy ownership transfer.
///
/// `PyBackedBytes` borrows Python's buffer; `Bytes::from_owner` wraps it for
/// hyper. Python's refcount keeps the buffer alive until Rust drops it.
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
///
/// Binary frames use `PyBackedBytes` + `Bytes::from_owner` for zero-copy
/// transfer from Python to tungstenite.
fn parse_ws_send(event: &Bound<'_, PyDict>) -> PyResult<AsgiEvent> {
    let py = event.py();
    let text: Option<String> = event
        .get_item(pyo3::intern!(py, "text"))?
        .and_then(|v| v.extract().ok());
    let bytes: Option<Bytes> = match event.get_item(pyo3::intern!(py, "bytes"))? {
        Some(v) => match v.cast::<PyBytes>() {
            Ok(py_bytes) => {
                let backed: PyBackedBytes = py_bytes.clone().into();
                Some(Bytes::from_owner(backed))
            }
            Err(_) => None,
        },
        None => None,
    };
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
    // Direct C-API indexing (PyList_GET_ITEM) avoids Python iterator protocol overhead.
    let list: &Bound<'_, PyList> = list.cast()?;
    let len = list.len();
    let mut headers = HeaderMap::with_capacity(len);
    for i in 0..len {
        let tuple = list.get_item(i)?;
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

// ── scope_from_template ──────────────────────────────────────────────────

/// Build an HTTP scope from the pre-populated template.
///
/// `dict.copy()` + per-request fields only. For HTTP/1.1 (>95% of traffic),
/// the `http_version` field is already correct from the template.
pub fn scope_from_template(
    py: Python<'_>,
    template: &Py<PyDict>,
    request: &InboundRequest,
    fastapi_app: Option<&Py<PyAny>>,
    interns: &ScopeInterns,
) -> PyResult<Py<PyDict>> {
    let scope = template
        .bind(py)
        .call_method0(pyo3::intern!(py, "copy"))?
        .cast_into::<PyDict>()
        .map_err(|e| {
            pyo3::exceptions::PyTypeError::new_err(format!(
                "scope template copy returned non-dict: {e}"
            ))
        })?;
    if request.protocol != ProtocolVersion::Http11 {
        scope.set_item(
            interns.keys.http_version.bind(py),
            interns.versions.get(py, request.protocol),
        )?;
    }
    set_scope_request_fields(py, &scope, request, interns)?;
    set_scope_headers(py, &scope, request, interns)?;
    set_scope_addresses(py, &scope, request, interns)?;
    set_scope_path_params(py, &scope, request, interns)?;
    scope.set_item(interns.keys.state.bind(py), PyDict::new(py))?;
    if let Some(app) = fastapi_app {
        scope.set_item(interns.keys.app.bind(py), app.bind(py))?;
        scope.set_item(
            interns.keys.router.bind(py),
            app.bind(py).getattr(c"router")?,
        )?;
    }
    Ok(scope.unbind())
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

/// Set request-specific scope fields: http_version, method, path, raw_path, query_string.
fn set_scope_request_fields(
    py: Python<'_>,
    dict: &Bound<'_, PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<()> {
    dict.set_item(
        interns.keys.http_version.bind(py),
        interns.versions.get(py, request.protocol),
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
    dict.set_item(interns.keys.server.bind(py), interns.server_tuple.bind(py))?;
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

    const TEST_SERVER_ADDR: SocketAddr =
        SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);
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
        with_py(|py| {
            let (tx, _rx) = oneshot::channel();
            let (dtx, _drx) = oneshot::channel();
            let cache = SendCache::new(py).unwrap();
            let send = AsgiSend::http(tx, dtx, &cache, py);
            let dbg = format!("{send:?}");
            assert!(dbg.contains("AsgiSend::Http"));
        });
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
                let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
                let scope =
                    scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let scope =
                scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
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

        with_py(|py| {
            let cache = SendCache::new(py).unwrap();
            let mut send = AsgiSend::http(response_tx, disconnect_tx, &cache, py);

            let start_dict = PyDict::new(py);
            start_dict.set_item("type", "http.response.start").unwrap();
            start_dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            start_dict.set_item("headers", headers).unwrap();
            send.__call__(py, start_dict.clone()).unwrap();

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

        with_py(|py| {
            let cache = SendCache::new(py).unwrap();
            let mut send = AsgiSend::http(response_tx, disconnect_tx, &cache, py);

            let start_dict = PyDict::new(py);
            start_dict.set_item("type", "http.response.start").unwrap();
            start_dict.set_item("status", 200u16).unwrap();
            let headers = PyList::empty(py);
            start_dict.set_item("headers", headers).unwrap();
            send.__call__(py, start_dict.clone()).unwrap();

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
                    assert_eq!(bytes.as_deref(), Some(b"\x01\x02\x03".as_ref()));
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
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
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
                bytes: Some(Bytes::from_static(&[0x01, 0x02, 0x03])),
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

    // ── Microbenchmarks ─────────────────────────────────────────────────
    //
    // Run with: cargo test -p apx-framework -- --nocapture microbench
    //
    // These are not assert-based tests — they print timing comparisons
    // for manual inspection. They isolate specific operations to validate
    // (or refute) performance hypotheses.

    const MICROBENCH_ITERATIONS: usize = 100_000;

    fn bench_loop<F: FnMut()>(label: &str, mut f: F) -> std::time::Duration {
        // Warmup
        for _ in 0..1000 {
            f();
        }
        let start = std::time::Instant::now();
        for _ in 0..MICROBENCH_ITERATIONS {
            f();
        }
        let elapsed = start.elapsed();
        let per_op = elapsed / MICROBENCH_ITERATIONS as u32;
        eprintln!("  {label:40} {per_op:>8?}  ({elapsed:?} / {MICROBENCH_ITERATIONS})");
        elapsed
    }

    #[test]
    fn microbench_version_intern_vs_direct_str() {
        eprintln!("\n=== VersionInterns.get() vs direct as_asgi_version() ===");
        with_py(|py| {
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let key = interns.keys.http_version.bind(py);
            let dict = PyDict::new(py);
            let protocol = ProtocolVersion::Http11;

            bench_loop("VersionInterns.get(Http11) + set_item", || {
                dict.set_item(key, interns.versions.get(py, protocol))
                    .unwrap();
            });

            bench_loop("as_asgi_version() + set_item", || {
                dict.set_item(key, protocol.as_asgi_version()).unwrap();
            });
        });
    }

    #[test]
    fn microbench_server_tuple_cached_vs_dynamic() {
        eprintln!("\n=== Cached server_tuple vs dynamic ip().to_string() ===");
        with_py(|py| {
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let key = interns.keys.server.bind(py);
            let dict = PyDict::new(py);

            bench_loop("cached server_tuple + set_item", || {
                dict.set_item(key, interns.server_tuple.bind(py)).unwrap();
            });

            bench_loop("dynamic (ip.to_string(), port) + set_item", || {
                dict.set_item(
                    key,
                    (TEST_SERVER_ADDR.ip().to_string(), TEST_SERVER_ADDR.port()),
                )
                .unwrap();
            });
        });
    }

    #[test]
    fn microbench_resolved_awaitable_singleton_vs_freelist() {
        eprintln!("\n=== ResolvedAwaitable: clone_ref (singleton) vs Py::new (freelist) ===");
        with_py(|py| {
            let cache = SendCache::new(py).unwrap();

            bench_loop("clone_ref (singleton)", || {
                let _ = cache.resolved.clone_ref(py);
            });

            bench_loop("Py::new (freelist=128)", || {
                let _ = Py::new(py, ResolvedAwaitable).unwrap();
            });
        });
    }

    #[test]
    fn microbench_receive_dict_build_vs_template_copy() {
        eprintln!("\n=== Receive dict: direct build vs template.copy() ===");
        with_py(|py| {
            let body = PyBytes::new(py, b"hello world");

            // NEW: direct dict construction with interned keys
            bench_loop("direct PyDict + 3x set_item (interned)", || {
                let event = PyDict::new(py);
                event
                    .set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))
                    .unwrap();
                event.set_item(pyo3::intern!(py, "body"), &body).unwrap();
                event
                    .set_item(pyo3::intern!(py, "more_body"), false)
                    .unwrap();
                std::hint::black_box(&event);
            });

            // OLD: template.copy() + set_item(body)
            let template = PyDict::new(py);
            template
                .set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))
                .unwrap();
            template
                .set_item(pyo3::intern!(py, "body"), PyBytes::new(py, b""))
                .unwrap();
            template
                .set_item(pyo3::intern!(py, "more_body"), false)
                .unwrap();
            let template = template.unbind();

            bench_loop("template.copy() + set_item(body)", || {
                let event: Bound<'_, PyDict> = template
                    .bind(py)
                    .call_method0(pyo3::intern!(py, "copy"))
                    .unwrap()
                    .cast_into()
                    .unwrap();
                event.set_item(pyo3::intern!(py, "body"), &body).unwrap();
                std::hint::black_box(&event);
            });
        });
    }

    #[test]
    fn microbench_full_scope_build() {
        eprintln!("\n=== Full scope_from_template (new interns) ===");
        with_py(|py| {
            let interns = ScopeInterns::new(py, TEST_SERVER_ADDR);
            let req = make_inbound_request(
                http::Method::GET,
                "/api/health",
                b"",
                HeaderMap::new(),
                Vec::new(),
                Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
            );

            bench_loop("scope_from_template", || {
                let _ =
                    scope_from_template(py, &interns.scope_template, &req, None, &interns).unwrap();
            });
        });
    }

    #[test]
    fn microbench_pylist_direct_index_vs_iterator() {
        eprintln!("\n=== PyList: direct index vs try_iter() ===");
        with_py(|py| {
            let items: Vec<Bound<'_, PyAny>> = (0..20i32)
                .map(|i| {
                    PyTuple::new(
                        py,
                        [
                            PyBytes::new(py, format!("header-{i}").as_bytes()).into_any(),
                            PyBytes::new(py, format!("value-{i}").as_bytes()).into_any(),
                        ],
                    )
                    .unwrap()
                    .into_any()
                })
                .collect();
            let list = PyList::new(py, &items).unwrap();

            bench_loop("direct index: list.get_item(i)", || {
                let mut count = 0usize;
                for i in 0..list.len() {
                    let tuple = list.get_item(i).unwrap();
                    std::hint::black_box(&tuple);
                    count += 1;
                }
                std::hint::black_box(count);
            });

            bench_loop("try_iter() protocol", || {
                let mut count = 0usize;
                for item in list.try_iter().unwrap() {
                    let tuple = item.unwrap();
                    std::hint::black_box(&tuple);
                    count += 1;
                }
                std::hint::black_box(count);
            });
        });
    }
}
