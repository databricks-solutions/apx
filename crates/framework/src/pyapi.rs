//! Python-visible types exported via PyO3.
//!
//! These `#[pyclass]` definitions are registered into the `apx._core` extension
//! module and used both by Python user code and by the Rust discovery/dispatch
//! layers — eliminating the previous pattern of importing Python classes at
//! runtime and extracting attributes by string name.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyDictMethods};
use std::sync::OnceLock;

// ── Request ─────────────────────────────────────────────────────────────

/// Full HTTP request object, constructed by Rust for `RawRequest` injection.
///
/// Python: `apx._core.Request`
#[pyclass(module = "apx._core")]
pub struct Request {
    /// HTTP method.
    #[pyo3(get)]
    pub method: String,
    /// Request path.
    #[pyo3(get)]
    pub path: String,
    /// Query string (without leading `?`).
    #[pyo3(get)]
    pub query_string: String,
    /// HTTP headers dict.
    #[pyo3(get)]
    pub headers: Py<PyAny>,
    /// Cookies dict.
    #[pyo3(get)]
    pub cookies: Py<PyAny>,
    /// Raw body bytes.
    pub(crate) body_bytes: Py<PyAny>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[pymethods]
impl Request {
    /// Create a new Request.
    #[new]
    #[pyo3(signature = (*, method="GET".to_owned(), path="/".to_owned(), query_string=String::new(), headers=None, cookies=None, body=None))]
    fn new(
        py: Python<'_>,
        method: String,
        path: String,
        query_string: String,
        headers: Option<Py<PyAny>>,
        cookies: Option<Py<PyAny>>,
        body: Option<&Bound<'_, PyBytes>>,
    ) -> Self {
        let headers = headers.unwrap_or_else(|| PyDict::new(py).into_any().unbind());
        let cookies = cookies.unwrap_or_else(|| PyDict::new(py).into_any().unbind());
        let body_bytes = match body {
            Some(b) => b.clone().into_any().unbind(),
            None => PyBytes::new(py, &[]).into_any().unbind(),
        };
        Self {
            method,
            path,
            query_string,
            headers,
            cookies,
            body_bytes,
        }
    }

    /// Return the pre-read request body bytes.
    ///
    /// Async for API compatibility with frameworks that read the body lazily.
    fn body<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        create_immediate_coro(py, self.body_bytes.bind(py))
    }

    fn __repr__(&self) -> String {
        format!("Request(method={:?}, path={:?})", self.method, self.path)
    }
}

/// Cached compiled Python function for immediate coroutines.
static IMMEDIATE_CORO_FN: OnceLock<Result<Py<PyAny>, String>> = OnceLock::new();

/// Create a Python coroutine that immediately returns a value.
///
/// Compiles the helper function once and caches it for subsequent calls.
fn create_immediate_coro<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let result = IMMEDIATE_CORO_FN.get_or_init(|| {
        let code = c"async def _immediate(v):\n    return v";
        let globals = PyDict::new(py);
        if let Err(e) = py.run(code, Some(&globals), None) {
            return Err(format!("{e}"));
        }
        match globals.get_item(c"_immediate") {
            Ok(Some(f)) => Ok(f.unbind()),
            Ok(None) => Err("_immediate not found after exec".to_owned()),
            Err(e) => Err(format!("{e}")),
        }
    });
    let func = result
        .as_ref()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.clone()))?;
    func.bind(py).call1((value,))
}

// ── Response ────────────────────────────────────────────────────────────

/// Raw HTTP response with explicit control over body, status, and headers.
///
/// Python: `apx._core.Response`
#[pyclass(module = "apx._core")]
pub struct Response {
    /// Response body (any JSON-serializable Python object).
    #[pyo3(get, set)]
    pub body: Py<PyAny>,
    /// HTTP status code.
    #[pyo3(get, set)]
    pub status: u16,
    /// Response headers.
    #[pyo3(get, set)]
    pub headers: Py<PyAny>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[pymethods]
impl Response {
    /// Create a new Response.
    #[new]
    #[pyo3(signature = (body=None, status=200, headers=None))]
    fn new(
        py: Python<'_>,
        body: Option<Py<PyAny>>,
        status: u16,
        headers: Option<Py<PyAny>>,
    ) -> Self {
        let body = body.unwrap_or_else(|| py.None());
        let headers = headers.unwrap_or_else(|| PyDict::new(py).into_any().unbind());
        Self {
            body,
            status,
            headers,
        }
    }

    fn __repr__(&self) -> String {
        format!("Response(status={})", self.status)
    }
}

// ── Exceptions ──────────────────────────────────────────────────────────

pyo3::create_exception!(
    apx._core,
    NotFound,
    PyException,
    "Return a 404 Not Found response."
);
pyo3::create_exception!(
    apx._core,
    BadRequest,
    PyException,
    "Return a 400 Bad Request response."
);
pyo3::create_exception!(
    apx._core,
    Forbidden,
    PyException,
    "Return a 403 Forbidden response."
);

// ── Module registration ─────────────────────────────────────────────────

/// Register all framework types and exceptions into the given Python module.
///
/// Called from the top-level `#[pymodule]` in `crates/apx/src/lib.rs`.
///
/// # Errors
///
/// Returns an error if any type or exception registration fails.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Request>()?;
    m.add_class::<Response>()?;
    m.add_class::<crate::bridge::asgi::AsgiReceive>()?;
    m.add_class::<crate::bridge::asgi::AsgiSend>()?;

    m.add("NotFound", m.py().get_type::<NotFound>())?;
    m.add("BadRequest", m.py().get_type::<BadRequest>())?;
    m.add("Forbidden", m.py().get_type::<Forbidden>())?;

    Ok(())
}
