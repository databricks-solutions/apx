//! ASGI dict templates: pre-built dicts with fixed fields.
//!
//! Templates are created once at worker startup and stored on `AppState`.
//! Per-request dispatch copies the template via `dict.copy()` and updates
//! only the variable fields, saving repeated `set_item` calls per request.

use super::asgi::ScopeInterns;
use crate::transport::types::InboundRequest;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::net::SocketAddr;

/// Build the receive-event template dict with fixed ASGI fields.
///
/// Called once per worker at startup. The returned dict is stored
/// on `AppState` and copied per request in `AsgiReceive::__call__`.
pub fn build_receive_template(py: Python<'_>) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item(pyo3::intern!(py, "type"), pyo3::intern!(py, "http.request"))?;
    dict.set_item(pyo3::intern!(py, "body"), PyBytes::new(py, b""))?;
    dict.set_item(pyo3::intern!(py, "more_body"), false)?;
    Ok(dict.unbind())
}

/// Build the scope template dict with fixed ASGI fields.
///
/// Called once per worker at startup. The returned dict is stored
/// on `AppState` and copied per request. Includes the server address
/// tuple so `scope_from_template` doesn't format it on every request.
pub fn build_scope_template(
    py: Python<'_>,
    interns: &ScopeInterns,
    fastapi_app: Option<&Py<PyAny>>,
    server_addr: SocketAddr,
) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
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

    // Server address — fixed for all requests on this worker.
    dict.set_item(
        interns.keys.server.bind(py),
        (server_addr.ip().to_string(), server_addr.port()),
    )?;
    // Client address defaults to None; overridden per-request when present.
    dict.set_item(interns.keys.client.bind(py), py.None())?;

    if let Some(app) = fastapi_app {
        dict.set_item(interns.keys.app.bind(py), app.bind(py))?;
        dict.set_item(
            interns.keys.router.bind(py),
            app.bind(py).getattr(c"router")?,
        )?;
    }
    Ok(dict.unbind())
}

/// Build a request scope from the template by copying and setting variable fields.
///
/// This replaces `build_http_scope` for the buffered dispatch path.
/// Saves ~6 `set_item` calls and `PyDict::new` per request.
pub fn scope_from_template(
    py: Python<'_>,
    template: &Py<PyDict>,
    request: &InboundRequest,
    interns: &ScopeInterns,
) -> PyResult<Py<PyDict>> {
    let trace = super::bench_trace_enabled();

    let t_copy = trace.then(std::time::Instant::now);
    let dict: Bound<'_, PyDict> = template
        .bind(py)
        .call_method0(pyo3::intern!(py, "copy"))?
        .cast_into()?;
    let copy_us = t_copy.map(|t| t.elapsed().as_micros());

    // Variable per-request fields.
    dict.set_item(
        interns.keys.http_version.bind(py),
        request.protocol.as_asgi_version(),
    )?;
    dict.set_item(interns.keys.method.bind(py), request.method.as_str())?;
    dict.set_item(
        interns.keys.path.bind(py),
        super::asgi::percent_decode(&request.path),
    )?;
    dict.set_item(
        interns.keys.raw_path.bind(py),
        PyBytes::new(py, request.path.as_bytes()),
    )?;
    dict.set_item(
        interns.keys.query_string.bind(py),
        PyBytes::new(py, &request.query_string),
    )?;

    // Headers.
    let t_headers = trace.then(std::time::Instant::now);
    let headers_list = PyList::empty(py);
    for (name, value) in &request.headers {
        let n = PyBytes::new(py, name.as_str().as_bytes());
        let v = PyBytes::new(py, value.as_bytes());
        let pair = PyTuple::new(py, [n.into_any(), v.into_any()])?;
        headers_list.append(pair)?;
    }
    dict.set_item(interns.keys.headers.bind(py), headers_list)?;
    let headers_us = t_headers.map(|t| t.elapsed().as_micros());

    // Client address — override the template's default None when present.
    // Server address is already baked into the template at startup.
    if let Some(addr) = request.client_addr {
        dict.set_item(
            interns.keys.client.bind(py),
            (addr.ip().to_string(), addr.port()),
        )?;
    }

    // Path params.
    let t_params = trace.then(std::time::Instant::now);
    let pp = PyDict::new(py);
    for (k, v) in &request.path_params {
        pp.set_item(k.as_str(), super::asgi::percent_decode(v.as_str()))?;
    }
    dict.set_item(interns.keys.path_params.bind(py), pp)?;
    let params_us = t_params.map(|t| t.elapsed().as_micros());

    // Fresh state dict per request.
    dict.set_item(interns.keys.state.bind(py), PyDict::new(py))?;

    if trace {
        tracing::info!(
            target: "bench_trace",
            phase = "scope_from_template",
            dict_copy_us = copy_us.unwrap_or(0),
            headers_us = headers_us.unwrap_or(0),
            path_params_us = params_us.unwrap_or(0),
            header_count = request.headers.len(),
        );
    }

    Ok(dict.unbind())
}
