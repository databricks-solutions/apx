//! Direct dispatch — calls Python handlers without ASGI overhead.
//!
//! Bypasses the scope/receive/send pipeline entirely. Rust extracts
//! parameters from [`InboundRequest`], calls the handler with a kwargs
//! dict, and serializes the return value to JSON. Routes with
//! `Depends()` continue using [`super::asgi_dispatch::AsgiBridgeDispatch`].

use crate::bridge::bench_trace_enabled;
use crate::bridge::dispatch::{AppState, HandlerDispatch};
use crate::error::AppError;
use crate::route::{BoundRoute, DirectContext, ParamSource, ResponseType};
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::StatusCode;
use http::header::HeaderMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyString};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Direct dispatch — calls handlers with Rust-extracted parameters.
pub struct DirectDispatch;

impl std::fmt::Debug for DirectDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectDispatch").finish()
    }
}

impl HandlerDispatch for DirectDispatch {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<axum::response::Response, AppError>> + Send>> {
        Box::pin(async move {
            tracing::debug!(
                path = %request.path,
                handler = %route.manifest.handler_qualname,
                "direct_dispatch: handle entry"
            );

            // 1. Take body + collect bytes (async, before GIL).
            let body_bytes = request
                .take_body()
                .collect(app_state.max_body_limit.0)
                .await
                .map_err(|_| AppError::BodyParse(crate::error::BodyParseKind::BodyTooLarge))?;

            // 2. Branch on sync vs async handler.
            let response = if route.manifest.is_async_handler {
                dispatch_async(route, app_state, request, body_bytes).await?
            } else {
                dispatch_sync(route, request, body_bytes)?
            };
            Ok(crate::transport::convert::to_axum_response(response))
        })
    }
}

// ── Sync handler path ─────────────────────────────────────────────────

/// Dispatch a sync handler entirely on the current thread via `Python::attach`.
fn dispatch_sync(
    route: Arc<BoundRoute>,
    request: InboundRequest,
    body_bytes: Bytes,
) -> Result<OutboundResponse, AppError> {
    let trace = bench_trace_enabled();
    let t_total = trace.then(std::time::Instant::now);

    let ctx = route
        .direct_context
        .as_ref()
        .ok_or_else(|| AppError::Internal("missing DirectContext on Direct route".to_owned()))?;

    let trace_ctx = crate::telemetry::context::extract_trace_context();

    let (status, json_bytes) = Python::attach(|py| {
        if let Some(ref ctx) = trace_ctx {
            let _ = crate::telemetry::context::set_python_context(py, ctx);
        }
        let kwargs = build_kwargs(py, &request, &route, &body_bytes, ctx)?;
        let result = route
            .handler
            .inner()
            .call(py, (), Some(kwargs.bind(py)))
            .map_err(|e| classify_handler_error(py, &e, ctx))?;
        serialize_response(py, result.bind(py), ctx, &route.manifest.response_type)
    })?;

    let response = build_outbound_response(status, json_bytes);

    if let Some(t_total) = t_total {
        tracing::info!(
            target: "bench_trace",
            phase = "direct_dispatch_sync",
            total_us = t_total.elapsed().as_micros(),
        );
    }

    Ok(response)
}

// ── Async handler path ────────────────────────────────────────────────

/// Dispatch an async handler via the event loop.
async fn dispatch_async(
    route: Arc<BoundRoute>,
    app_state: Arc<AppState>,
    request: InboundRequest,
    body_bytes: Bytes,
) -> Result<OutboundResponse, AppError> {
    let trace = bench_trace_enabled();
    let t_total = trace.then(std::time::Instant::now);

    let ctx = route
        .direct_context
        .as_ref()
        .ok_or_else(|| AppError::Internal("missing DirectContext on Direct route".to_owned()))?;

    // Build kwargs on the current thread (brief GIL hold).
    let kwargs = Python::attach(|py| build_kwargs(py, &request, &route, &body_bytes, ctx))?;

    // Extract trace context before crossing to the event loop thread.
    let trace_ctx = crate::telemetry::context::extract_trace_context();

    // Clone Arcs for the closure.
    let route_inner = Arc::clone(&route);

    // Schedule the handler coroutine on the event loop thread.
    let handler_rx = app_state.loop_handle.schedule_deferred(move |py| {
        if let Some(ref ctx) = trace_ctx {
            let _ = crate::telemetry::context::set_python_context(py, ctx);
        }
        let ctx = route_inner.direct_context.as_ref().ok_or_else(|| {
            AppError::Internal("missing DirectContext on Direct route".to_owned())
        })?;

        let result = route_inner
            .handler
            .inner()
            .call(py, (), Some(kwargs.bind(py)))
            .map_err(|e| AppError::Internal(format!("handler call: {e}")))?;

        // The result is a coroutine — wrap it in an async wrapper that serializes.
        let wrapper = build_async_wrapper(py, result, ctx, &route_inner.manifest.response_type)?;
        Ok(wrapper)
    })?;

    let py_result = handler_rx
        .await
        .map_err(|_| AppError::Internal("event loop closed before coroutine completed".to_owned()))?
        .map_err(|e| match e {
            AppError::Internal(msg) => AppError::Internal(msg),
            other => other,
        })?;

    // The wrapper coroutine returns a Python tuple (status_code, json_bytes).
    let (status, json_bytes) = Python::attach(|py| extract_async_result(py, &py_result))?;

    let response = build_outbound_response(status, json_bytes);

    if let Some(t_total) = t_total {
        tracing::info!(
            target: "bench_trace",
            phase = "direct_dispatch_async",
            total_us = t_total.elapsed().as_micros(),
        );
    }

    Ok(response)
}

/// Build an async wrapper coroutine that awaits the handler and serializes the result.
///
/// Returns a Python coroutine that the event loop scheduler can drive.
fn build_async_wrapper(
    py: Python<'_>,
    handler_coro: Py<PyAny>,
    ctx: &DirectContext,
    response_type: &ResponseType,
) -> Result<Py<PyAny>, AppError> {
    // We need to build a Python coroutine wrapper. Use a small inline Python snippet.
    let wrapper_code = c"
async def _wrap(coro, serialize_fn):
    result = await coro
    return serialize_fn(result)
";
    let globals = PyDict::new(py);
    py.run(wrapper_code, Some(&globals), None)
        .map_err(|e| AppError::Internal(format!("build async wrapper code: {e}")))?;
    let wrap_fn = globals
        .get_item("_wrap")
        .map_err(|e| AppError::Internal(format!("get _wrap function: {e}")))?
        .ok_or_else(|| AppError::Internal("_wrap function not found in globals".to_owned()))?;

    // Build the serialize closure as a Python callable.
    let serialize_fn = build_serialize_closure(py, ctx, response_type)?;

    wrap_fn
        .call1((handler_coro, serialize_fn))
        .map(|v| v.unbind())
        .map_err(|e| AppError::Internal(format!("call _wrap: {e}")))
}

/// Build a Python closure that serializes a handler result to `(status_code, json_bytes)`.
fn build_serialize_closure(
    py: Python<'_>,
    ctx: &DirectContext,
    response_type: &ResponseType,
) -> Result<Py<PyAny>, AppError> {
    let status_code = match response_type {
        ResponseType::Model { status_code, .. } => *status_code,
        _ => 200,
    };

    // Create a closure using PyCFunction
    let json_dumps = ctx.json_dumps.clone_ref(py);
    let response_adapter = ctx.response_adapter.as_ref().map(|a| a.clone_ref(py));
    let http_exception_cls = ctx.http_exception_cls.clone_ref(py);

    let closure_code = if response_adapter.is_some() {
        c"
def _make_serializer(adapter, status_code, http_exc_cls):
    def serialize(result):
        if isinstance(result, BaseException):
            if isinstance(result, http_exc_cls):
                import json
                detail = getattr(result, 'detail', str(result))
                sc = getattr(result, 'status_code', 500)
                body = json.dumps({'type': 'about:blank', 'title': 'HTTP Error', 'status': sc, 'detail': detail}).encode()
                return (sc, body)
            raise result
        if result is None:
            return (204, b'')
        validated = adapter.dump_json(adapter.validate_python(result))
        return (status_code, bytes(validated))
    return serialize
"
    } else {
        c"
def _make_serializer(json_dumps_fn, status_code, http_exc_cls):
    def serialize(result):
        if isinstance(result, BaseException):
            if isinstance(result, http_exc_cls):
                import json
                detail = getattr(result, 'detail', str(result))
                sc = getattr(result, 'status_code', 500)
                body = json.dumps({'type': 'about:blank', 'title': 'HTTP Error', 'status': sc, 'detail': detail}).encode()
                return (sc, body)
            raise result
        if result is None:
            return (204, b'')
        return (status_code, json_dumps_fn(result).encode())
    return serialize
"
    };

    let globals = PyDict::new(py);
    py.run(closure_code, Some(&globals), None)
        .map_err(|e| AppError::Internal(format!("compile serializer: {e}")))?;
    let make_serializer = globals
        .get_item("_make_serializer")
        .map_err(|e| AppError::Internal(format!("get _make_serializer: {e}")))?
        .ok_or_else(|| AppError::Internal("_make_serializer not found".to_owned()))?;

    let serializer = if let Some(adapter) = response_adapter {
        make_serializer.call1((adapter, status_code, http_exception_cls))
    } else {
        make_serializer.call1((json_dumps, status_code, http_exception_cls))
    }
    .map_err(|e| AppError::Internal(format!("build serializer: {e}")))?;

    Ok(serializer.unbind())
}

/// Extract `(status_code, json_bytes)` from the async wrapper result.
fn extract_async_result(py: Python<'_>, result: &Py<PyAny>) -> Result<(u16, Bytes), AppError> {
    let tuple = result.bind(py);
    let status: u16 = tuple
        .get_item(0)
        .and_then(|v| v.extract())
        .map_err(|e| AppError::Internal(format!("extract status from async result: {e}")))?;
    let body: Vec<u8> = tuple
        .get_item(1)
        .and_then(|v| v.extract())
        .map_err(|e| AppError::Internal(format!("extract body from async result: {e}")))?;
    Ok((status, Bytes::from(body)))
}

// ── Parameter extraction ──────────────────────────────────────────────

/// Convert a string value to a Python object based on the type's qualified name.
pub fn convert_scalar<'py>(
    py: Python<'py>,
    value: &str,
    type_qualname: &str,
) -> Result<Bound<'py, PyAny>, AppError> {
    match type_qualname {
        "int" => {
            let n: i64 = value
                .parse()
                .map_err(|_| AppError::BodyParse(crate::error::BodyParseKind::InvalidJson))?;
            Ok(n.into_pyobject(py)
                .map_err(|e| AppError::Internal(format!("convert int: {e}")))?
                .into_any())
        }
        "float" => {
            let n: f64 = value
                .parse()
                .map_err(|_| AppError::BodyParse(crate::error::BodyParseKind::InvalidJson))?;
            Ok(PyFloat::new(py, n).into_any())
        }
        "bool" => {
            let b = matches!(value, "true" | "True" | "1" | "yes");
            let py_bool = pyo3::types::PyBool::new(py, b);
            Ok(py_bool.to_owned().into_any())
        }
        // Default: treat as string (covers "str" and unknown types).
        _ => Ok(PyString::new(py, value).into_any()),
    }
}

/// Build a kwargs `PyDict` from the inbound request and route parameters.
fn build_kwargs(
    py: Python<'_>,
    request: &InboundRequest,
    route: &BoundRoute,
    body_bytes: &Bytes,
    ctx: &DirectContext,
) -> Result<Py<PyDict>, AppError> {
    let kwargs = PyDict::new(py);
    let mut body_validator_idx = 0;

    for param in &route.manifest.params {
        let value = resolve_param(py, request, param, body_bytes, ctx, &mut body_validator_idx)?;
        kwargs
            .set_item(&param.name, value)
            .map_err(|e| AppError::Internal(format!("set kwarg '{}': {e}", param.name)))?;
    }

    Ok(kwargs.unbind())
}

/// Resolve a single parameter value from the request.
fn resolve_param<'py>(
    py: Python<'py>,
    request: &InboundRequest,
    param: &crate::route::ParamManifest,
    body_bytes: &Bytes,
    ctx: &DirectContext,
    body_validator_idx: &mut usize,
) -> Result<Bound<'py, PyAny>, AppError> {
    match param.source {
        ParamSource::Path => resolve_path_param(py, request, param),
        ParamSource::Query => resolve_query_param(py, request, param),
        ParamSource::Header => resolve_header_param(py, request, param),
        ParamSource::Cookie => resolve_cookie_param(py, request, param),
        ParamSource::Body => resolve_body_param(py, body_bytes, ctx, body_validator_idx),
        ParamSource::RawBody => {
            let bytes_obj = pyo3::types::PyBytes::new(py, body_bytes);
            Ok(bytes_obj.into_any())
        }
    }
}

/// Resolve a path parameter.
fn resolve_path_param<'py>(
    py: Python<'py>,
    request: &InboundRequest,
    param: &crate::route::ParamManifest,
) -> Result<Bound<'py, PyAny>, AppError> {
    let wire_name = param.alias.as_deref().unwrap_or(&param.name);
    let value = request
        .path_params
        .iter()
        .find(|(k, _)| k == wire_name)
        .map(|(_, v)| v.as_str());

    match value {
        Some(v) => convert_scalar(py, v, param.type_qualname.as_str()),
        None if !param.required => resolve_default(py, param),
        None => Err(AppError::BodyParse(
            crate::error::BodyParseKind::InvalidJson,
        )),
    }
}

/// Resolve a query parameter.
fn resolve_query_param<'py>(
    py: Python<'py>,
    request: &InboundRequest,
    param: &crate::route::ParamManifest,
) -> Result<Bound<'py, PyAny>, AppError> {
    let wire_name = param.alias.as_deref().unwrap_or(&param.name);
    let value = form_urlencoded::parse(&request.query_string)
        .find(|(k, _)| k == wire_name)
        .map(|(_, v)| v.into_owned());

    match value {
        Some(v) => convert_scalar(py, &v, param.type_qualname.as_str()),
        None if !param.required => resolve_default(py, param),
        None => Err(AppError::BodyParse(
            crate::error::BodyParseKind::InvalidJson,
        )),
    }
}

/// Resolve a header parameter.
fn resolve_header_param<'py>(
    py: Python<'py>,
    request: &InboundRequest,
    param: &crate::route::ParamManifest,
) -> Result<Bound<'py, PyAny>, AppError> {
    let wire_name = param.alias.as_deref().unwrap_or(&param.name);
    let value = request.headers.get(wire_name).and_then(|v| v.to_str().ok());

    match value {
        Some(v) => convert_scalar(py, v, param.type_qualname.as_str()),
        None if !param.required => resolve_default(py, param),
        None => Err(AppError::BodyParse(
            crate::error::BodyParseKind::InvalidJson,
        )),
    }
}

/// Resolve a cookie parameter.
fn resolve_cookie_param<'py>(
    py: Python<'py>,
    request: &InboundRequest,
    param: &crate::route::ParamManifest,
) -> Result<Bound<'py, PyAny>, AppError> {
    let wire_name = param.alias.as_deref().unwrap_or(&param.name);
    let cookie_header = request
        .headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let value = cookie_header.split("; ").find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == wire_name).then(|| v.trim().to_owned())
    });

    match value {
        Some(v) => convert_scalar(py, &v, param.type_qualname.as_str()),
        None if !param.required => resolve_default(py, param),
        None => Err(AppError::BodyParse(
            crate::error::BodyParseKind::InvalidJson,
        )),
    }
}

/// Resolve a body parameter via Pydantic `model_validate_json`.
fn resolve_body_param<'py>(
    py: Python<'py>,
    body_bytes: &Bytes,
    ctx: &DirectContext,
    idx: &mut usize,
) -> Result<Bound<'py, PyAny>, AppError> {
    let (_name, model_cls) = ctx
        .body_validators
        .get(*idx)
        .ok_or_else(|| AppError::Internal("body validator index out of range".to_owned()))?;
    *idx += 1;

    let py_bytes = pyo3::types::PyBytes::new(py, body_bytes.as_ref());
    model_cls
        .call_method1(py, pyo3::intern!(py, "model_validate_json"), (py_bytes,))
        .map(|v| v.into_bound(py))
        .map_err(|e| {
            AppError::Validation(vec![crate::error::ValidationErrorItem {
                loc: vec!["body".to_owned()],
                msg: format!("{e}"),
                r#type: "value_error".to_owned(),
            }])
        })
}

/// Resolve a parameter's default value from its JSON representation.
fn resolve_default<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
) -> Result<Bound<'py, PyAny>, AppError> {
    match &param.default_json {
        Some(default_val) => {
            let json_mod = py
                .import(c"json")
                .map_err(|e| AppError::Internal(format!("import json: {e}")))?;
            let json_str = default_val.to_string();
            json_mod
                .call_method1(c"loads", (json_str,))
                .map_err(|e| AppError::Internal(format!("parse default JSON: {e}")))
        }
        None => Ok(py.None().into_bound(py)),
    }
}

// ── Response serialization ────────────────────────────────────────────

/// Serialize the handler result to `(status_code, json_bytes)`.
pub fn serialize_response(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    ctx: &DirectContext,
    response_type: &ResponseType,
) -> Result<(u16, Bytes), AppError> {
    // None → 204 No Content
    if result.is_none() {
        let status = match response_type {
            ResponseType::Model { status_code, .. } => *status_code,
            _ => 204,
        };
        return Ok((status, Bytes::new()));
    }

    let status_code = match response_type {
        ResponseType::Model { status_code, .. } => *status_code,
        _ => 200,
    };

    let json_bytes = if let Some(ref adapter) = ctx.response_adapter {
        // TypeAdapter path: adapter.dump_json(adapter.validate_python(result))
        let validated = adapter
            .call_method1(py, pyo3::intern!(py, "validate_python"), (result,))
            .map_err(|e| AppError::Internal(format!("validate_python: {e}")))?;
        let dumped = adapter
            .call_method1(py, pyo3::intern!(py, "dump_json"), (validated,))
            .map_err(|e| AppError::Internal(format!("dump_json: {e}")))?;
        let bytes: Vec<u8> = dumped
            .extract(py)
            .map_err(|e| AppError::Internal(format!("extract json bytes: {e}")))?;
        Bytes::from(bytes)
    } else {
        // json.dumps path: for dict/list returns without response_model
        let json_str: String = ctx
            .json_dumps
            .call1(py, (result,))
            .and_then(|v| v.extract(py))
            .map_err(|e| AppError::Internal(format!("json.dumps: {e}")))?;
        Bytes::from(json_str.into_bytes())
    };

    Ok((status_code, json_bytes))
}

// ── Error classification ──────────────────────────────────────────────

/// Classify a Python exception from the handler.
pub fn classify_handler_error(py: Python<'_>, err: &PyErr, ctx: &DirectContext) -> AppError {
    let exc = err.value(py);
    if exc
        .is_instance(ctx.http_exception_cls.bind(py))
        .unwrap_or(false)
    {
        let status = exc
            .getattr(pyo3::intern!(py, "status_code"))
            .and_then(|v| v.extract::<u16>())
            .unwrap_or(500);
        let detail = exc
            .getattr(pyo3::intern!(py, "detail"))
            .and_then(|v| v.extract::<String>())
            .unwrap_or_else(|_| "Internal Server Error".to_owned());
        return AppError::HttpException { status, detail };
    }
    AppError::Internal(format!("{err}"))
}

// ── Response builder ──────────────────────────────────────────────────

/// Build an `OutboundResponse` from status code and JSON body bytes.
pub fn build_outbound_response(status: u16, body: Bytes) -> OutboundResponse {
    let mut headers = HeaderMap::with_capacity(1);
    if !body.is_empty() {
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
    }
    OutboundResponse {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        headers,
        body: ResponseBody::Fixed(body),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::with_py;

    #[test]
    fn convert_scalar_int() {
        with_py(|py| {
            let result = convert_scalar(py, "42", "int").unwrap();
            let val: i64 = result.extract().unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn convert_scalar_str() {
        with_py(|py| {
            let result = convert_scalar(py, "hello", "str").unwrap();
            let val: String = result.extract().unwrap();
            assert_eq!(val, "hello");
        });
    }

    #[test]
    fn convert_scalar_float() {
        with_py(|py| {
            let result = convert_scalar(py, "2.72", "float").unwrap();
            let val: f64 = result.extract().unwrap();
            assert!((val - 2.72).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn convert_scalar_bool_true() {
        with_py(|py| {
            let result = convert_scalar(py, "true", "bool").unwrap();
            let val: bool = result.extract().unwrap();
            assert!(val);
        });
    }

    #[test]
    fn convert_scalar_bool_false() {
        with_py(|py| {
            let result = convert_scalar(py, "false", "bool").unwrap();
            let val: bool = result.extract().unwrap();
            assert!(!val);
        });
    }

    #[test]
    fn convert_scalar_invalid_int() {
        with_py(|py| {
            let result = convert_scalar(py, "abc", "int");
            assert!(result.is_err());
        });
    }

    #[test]
    fn direct_dispatch_debug() {
        let d = DirectDispatch;
        let dbg = format!("{d:?}");
        assert!(dbg.contains("DirectDispatch"));
    }

    #[test]
    fn build_outbound_response_json() {
        let resp = build_outbound_response(200, Bytes::from(r#"{"ok":true}"#));
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );
        match resp.body {
            ResponseBody::Fixed(b) => assert_eq!(b.as_ref(), br#"{"ok":true}"#),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }

    #[test]
    fn build_outbound_response_empty() {
        let resp = build_outbound_response(204, Bytes::new());
        assert_eq!(resp.status, StatusCode::NO_CONTENT);
        assert!(resp.headers.get("content-type").is_none());
        match resp.body {
            ResponseBody::Fixed(b) => assert!(b.is_empty()),
            ResponseBody::Stream(_) => panic!("expected Fixed body"),
        }
    }
}
