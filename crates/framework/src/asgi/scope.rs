//! ASGI scope interning and template building.
//!
//! Provides [`ScopeInterns`] for pre-building scope dictionaries and
//! [`ResolvedAwaitable`] / [`ResolvedAwaitableWithValue`] for zero-overhead
//! Python awaitables.

use std::collections::HashMap;
use std::net::SocketAddr;

use crate::transport::types::ProtocolVersion;
use http::header::{self, HeaderName};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};

use super::{ASGI_SPEC_VERSION, ASGI_VERSION};

/// Default HTTP scheme (TLS detection is a future extension).
const DEFAULT_SCHEME: &str = "http";

// ── ScopeInterns ─────────────────────────────────────────────────────────

crate::opaque_debug!(ScopeInterns);

/// Pre-interned Python strings for ASGI scope construction.
///
/// Created once at worker startup, shared across all requests.
/// Eliminates ~25 transient `PyString` allocations per request.
pub struct ScopeInterns {
    /// Fixed keys used in every ASGI scope dict.
    pub(crate) keys: ScopeKeys,
    /// Cached `PyBytes` for common HTTP header names.
    pub(crate) headers: HeaderInterns,
    /// Pre-built `(host_str, port)` tuple for the server address.
    pub(crate) server_tuple: Py<PyTuple>,
    /// Cached `PyString` for HTTP protocol versions.
    pub(crate) versions: VersionInterns,
    /// Pre-built HTTP scope dict with fixed fields. `dict.copy()` per request.
    pub(crate) scope_template: Py<PyDict>,
    /// Shared empty dict for parameterless routes.
    pub(crate) empty_dict: Py<PyDict>,
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
}

crate::opaque_debug!(ScopeKeys);

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

/// Pre-built `PyBytes` for common HTTP header names, keyed by
/// lowercase header bytes for O(1) lookup.
pub struct HeaderInterns {
    pub(crate) map: HashMap<Box<[u8]>, Py<PyBytes>>,
}

crate::opaque_debug!(HeaderInterns);

impl HeaderInterns {
    /// Create cached `PyBytes` for common header names. Call once at worker startup.
    pub fn new(py: Python<'_>) -> Self {
        let map = COMMON_HEADERS
            .iter()
            .map(|h| {
                let key: Box<[u8]> = h.as_str().as_bytes().into();
                let val = PyBytes::new(py, h.as_str().as_bytes()).unbind();
                (key, val)
            })
            .collect();
        Self { map }
    }
}

/// Pre-interned `PyString` for HTTP protocol versions ("1.0", "1.1", "2").
pub struct VersionInterns {
    http10: Py<PyString>,
    http11: Py<PyString>,
    h2: Py<PyString>,
}

crate::opaque_debug!(VersionInterns);

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
        };
        let versions = VersionInterns::new(py);

        let scope_template = {
            let tpl = PyDict::new(py);
            let _ = tpl.set_item(keys.r#type.bind(py), pyo3::intern!(py, "http"));
            let _ = tpl.set_item(keys.asgi.bind(py), &asgi_dict);
            let _ = tpl.set_item(keys.scheme.bind(py), pyo3::intern!(py, DEFAULT_SCHEME));
            let _ = tpl.set_item(keys.root_path.bind(py), pyo3::intern!(py, ""));
            let _ = tpl.set_item(keys.http_version.bind(py), versions.http11.bind(py));
            let _ = tpl.set_item(keys.server.bind(py), server_tuple.bind(py));
            tpl.unbind()
        };

        Self {
            keys,
            headers: HeaderInterns::new(py),
            server_tuple,
            versions,
            scope_template,
            empty_dict: PyDict::new(py).unbind(),
        }
    }
}

// ── ResolvedAwaitable ────────────────────────────────────────────────────

/// Zero-overhead Python awaitable that completes immediately.
///
/// Used by the response writer to return from `send()` without scheduling.
/// Implements the Python iterator protocol so `await resolved` returns
/// `None` with no scheduling overhead.
#[expect(clippy::redundant_pub_crate, reason = "used from protocol::writer")]
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
        None
    }
}

/// Zero-overhead Python awaitable that completes immediately with a value.
///
/// Used by [`HttpReceive`](crate::protocol::connection::HttpReceive)
/// to return the receive dict without scheduling.
#[expect(clippy::redundant_pub_crate, reason = "used from protocol::connection")]
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
        let val = self
            .value
            .take()
            .unwrap_or_else(|| Python::attach(|py| py.None()));
        Err(pyo3::exceptions::PyStopIteration::new_err((val,)))
    }
}
