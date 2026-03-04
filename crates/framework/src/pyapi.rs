// PyO3 proc macros (#[pyclass], create_exception!) generate `unsafe impl` blocks.
#![allow(unsafe_code)]

//! Python-visible types exported via PyO3.
//!
//! These `#[pyclass]` definitions are registered into the `apx._core` extension
//! module and used both by Python user code and by the Rust discovery/dispatch
//! layers — eliminating the previous pattern of importing Python classes at
//! runtime and extracting attributes by string name.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyDictMethods};

// ── HTTP method enum ────────────────────────────────────────────────────

/// HTTP method for a route.
///
/// Mirrors [`crate::route::HttpMethod`] but is visible to Python as
/// `apx._core.HttpMethod`.
#[pyclass(eq, frozen, hash, from_py_object, module = "apx._core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyHttpMethod {
    /// GET
    Get,
    /// POST
    Post,
    /// PUT
    Put,
    /// DELETE
    Delete,
    /// PATCH
    Patch,
}

#[pymethods]
impl PyHttpMethod {
    /// String value matching the Python `HttpMethod` enum (e.g. `"GET"`).
    #[getter]
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn value(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
        }
    }
}

impl From<PyHttpMethod> for crate::route::HttpMethod {
    fn from(m: PyHttpMethod) -> Self {
        match m {
            PyHttpMethod::Get => Self::Get,
            PyHttpMethod::Post => Self::Post,
            PyHttpMethod::Put => Self::Put,
            PyHttpMethod::Delete => Self::Delete,
            PyHttpMethod::Patch => Self::Patch,
        }
    }
}

impl From<crate::route::HttpMethod> for PyHttpMethod {
    fn from(m: crate::route::HttpMethod) -> Self {
        match m {
            crate::route::HttpMethod::Get => Self::Get,
            crate::route::HttpMethod::Post => Self::Post,
            crate::route::HttpMethod::Put => Self::Put,
            crate::route::HttpMethod::Delete => Self::Delete,
            crate::route::HttpMethod::Patch => Self::Patch,
        }
    }
}

// ── Parameter info ──────────────────────────────────────────────────────

/// Parameter metadata extracted from a handler's signature.
///
/// Python: `apx._core.ParamInfo`
#[pyclass(frozen, from_py_object, module = "apx._core")]
#[derive(Debug, Clone)]
pub struct ParamInfo {
    /// Parameter name from the Python function signature.
    #[pyo3(get)]
    pub name: String,
    /// Qualified Python type name.
    #[pyo3(get)]
    pub type_qualname: String,
    /// Source: `"path"`, `"query"`, `"body"`, `"raw_body"`, `"raw_request"`.
    #[pyo3(get)]
    pub source: String,
    /// Whether the parameter is required (no default value).
    #[pyo3(get)]
    pub required: bool,
}

#[pymethods]
impl ParamInfo {
    /// Create a new `ParamInfo`.
    #[new]
    #[pyo3(signature = (name, type_qualname, source, required))]
    fn new(name: String, type_qualname: String, source: String, required: bool) -> Self {
        Self {
            name,
            type_qualname,
            source,
            required,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "ParamInfo(name={:?}, type_qualname={:?}, source={:?}, required={})",
            self.name, self.type_qualname, self.source, self.required
        )
    }
}

// ── Route info ──────────────────────────────────────────────────────────

/// Route metadata extracted from an `App` decorator.
///
/// Python: `apx._core.RouteInfo`
#[pyclass(frozen, module = "apx._core")]
pub struct RouteInfo {
    /// HTTP method.
    #[pyo3(get)]
    pub method: PyHttpMethod,
    /// URL path template.
    #[pyo3(get)]
    pub path: String,
    /// The actual async handler function.
    pub handler: Py<PyAny>,
    /// Qualified name of the handler.
    #[pyo3(get)]
    pub handler_qualname: String,
    /// Parameter metadata.
    #[pyo3(get)]
    pub params: Vec<ParamInfo>,
    /// Response type descriptor.
    #[pyo3(get)]
    pub response_type: String,
    /// Route tags.
    #[pyo3(get)]
    pub tags: Vec<String>,
}

impl std::fmt::Debug for RouteInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteInfo")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("handler_qualname", &self.handler_qualname)
            .field("params", &self.params)
            .field("response_type", &self.response_type)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

#[pymethods]
impl RouteInfo {
    /// Create a new `RouteInfo`.
    #[new]
    #[pyo3(signature = (method, path, handler, handler_qualname, params=vec![], response_type="raw_response".to_owned(), tags=vec![]))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        method: PyHttpMethod,
        path: String,
        handler: Py<PyAny>,
        handler_qualname: String,
        params: Vec<ParamInfo>,
        response_type: String,
        tags: Vec<String>,
    ) -> Self {
        Self {
            method,
            path,
            handler,
            handler_qualname,
            params,
            response_type,
            tags,
        }
    }

    /// Get the handler function.
    #[getter]
    fn handler(&self, py: Python<'_>) -> Py<PyAny> {
        self.handler.clone_ref(py)
    }

    fn __repr__(&self) -> String {
        format!(
            "RouteInfo(method={:?}, path={:?}, handler_qualname={:?})",
            self.method, self.path, self.handler_qualname
        )
    }
}

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
    #[allow(clippy::too_many_arguments)]
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

/// Create a Python coroutine that immediately returns a value.
fn create_immediate_coro<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let code = c"async def _immediate(v):\n    return v";
    let globals = PyDict::new(py);
    py.run(code, Some(&globals), None)?;
    let func = globals.get_item(c"_immediate")?.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("failed to create immediate coroutine")
    })?;
    func.call1((value,))
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
    m.add_class::<PyHttpMethod>()?;
    m.add_class::<ParamInfo>()?;
    m.add_class::<RouteInfo>()?;
    m.add_class::<Request>()?;
    m.add_class::<Response>()?;

    m.add("NotFound", m.py().get_type::<NotFound>())?;
    m.add("BadRequest", m.py().get_type::<BadRequest>())?;
    m.add("Forbidden", m.py().get_type::<Forbidden>())?;

    Ok(())
}
