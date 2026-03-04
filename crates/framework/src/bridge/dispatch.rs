//! Handler dispatch trait and request-response implementation.
//!
//! Each [`HandlerKind`](crate::route::HandlerKind) gets its own
//! [`HandlerDispatch`] impl, owning the full request lifecycle.

use super::context::RequestContext;
use crate::error::{AppError, ValidationErrorItem, map_body_error};
use crate::route::{BoundRoute, ParamSource, ResponseType};
use axum::response::Response;
use bytes::Bytes;
use http::StatusCode;
use pyo3::types::{PyAnyMethods, PyBytes, PyDict, PyDictMethods, PyString, PyTypeMethods};
use pyo3::{Py, PyAny, Python};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Lifecycle-scoped state shared across all routes in a single worker.
#[derive(Clone, Copy)]
pub struct AppState {
    /// Max request body size in bytes.
    pub max_body_limit: crate::route::BodyLimit,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("max_body_limit", &self.max_body_limit)
            .finish_non_exhaustive()
    }
}

/// Handles the full request lifecycle for a specific handler kind.
///
/// v0: only [`RequestResponseDispatch`].
pub trait HandlerDispatch: Send + Sync {
    /// Process a request and return an HTTP response.
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        path_params: Vec<(String, String)>,
        request: axum::extract::Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, AppError>> + Send>>;
}

/// Standard request → response dispatch via Python.
pub struct RequestResponseDispatch;

impl std::fmt::Debug for RequestResponseDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestResponseDispatch").finish()
    }
}

impl HandlerDispatch for RequestResponseDispatch {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        path_params: Vec<(String, String)>,
        request: axum::extract::Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, AppError>> + Send>> {
        Box::pin(async move {
            let ctx = extract_context(request, path_params, &route, &app_state).await?;
            let result = invoke_handler(&route, &ctx).await?;
            Python::attach(|py| serialize_result(py, &result, &route))
        })
    }
}

// ── Step 1: Extract HTTP parts ──────────────────────────────────────────

/// Extract HTTP parts into [`RequestContext`].
///
/// Path params are pre-extracted by axum's `RawPathParams` extractor
/// (percent-decoded). Query params are parsed via `form_urlencoded`.
async fn extract_context(
    request: axum::extract::Request,
    path_params: Vec<(String, String)>,
    route: &BoundRoute,
    app_state: &AppState,
) -> Result<RequestContext, AppError> {
    let (parts, body) = request.into_parts();

    let query_params = extract_query_params(&parts);

    let body = if route.has_body_param {
        let bytes = axum::body::to_bytes(body, app_state.max_body_limit.0)
            .await
            .map_err(map_body_error)?;
        Some(bytes)
    } else {
        None
    };

    Ok(RequestContext {
        path_params,
        query_params,
        headers: parts.headers,
        body,
    })
}

/// Parse query string into URL-decoded key-value pairs.
///
/// Uses `form_urlencoded` (same parser axum uses internally) for proper
/// percent-decoding and `+`-as-space handling.
fn extract_query_params(parts: &http::request::Parts) -> Vec<(String, String)> {
    parts
        .uri
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

// ── Step 2: Invoke handler ──────────────────────────────────────────────

/// Call the Python handler and await the result.
///
/// Phase 1 (GIL held): build kwargs, call handler, get coroutine, convert
/// to Rust future via `into_future`.
/// Phase 2 (GIL released): await the future.
async fn invoke_handler(route: &BoundRoute, ctx: &RequestContext) -> Result<Py<PyAny>, AppError> {
    let future = Python::attach(|py| {
        let kwargs = build_kwargs(py, route, ctx)?;
        let coro = route
            .handler
            .call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("handler call failed: {e}")))?;

        pyo3_async_runtimes::tokio::into_future(coro.into_bound(py))
            .map_err(|e| AppError::Internal(format!("into_future: {e}")))
    })?;

    future.await.map_err(python_err_to_app_error)
}

/// Build Python kwargs dict from route params and request context.
fn build_kwargs<'py>(
    py: Python<'py>,
    route: &BoundRoute,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyDict>, AppError> {
    let kwargs = PyDict::new(py);

    for param in &route.params {
        let value = resolve_param_value(py, &param.manifest, ctx, param.python_type.as_ref())?;
        kwargs
            .set_item(&param.manifest.name, value)
            .map_err(|e| AppError::Internal(format!("set kwarg: {e}")))?;
    }

    Ok(kwargs)
}

/// Resolve a single parameter value from the request context.
fn resolve_param_value<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
    ctx: &RequestContext,
    python_type: Option<&Py<PyAny>>,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    match param.source {
        ParamSource::Path => resolve_path_param(py, param, ctx),
        ParamSource::Query => resolve_query_param(py, param, ctx),
        // TODO(phase-3): header/cookie extraction via InboundRequest
        ParamSource::Header | ParamSource::Cookie => Err(AppError::Internal(format!(
            "param source {:?} not yet implemented",
            param.source
        ))),
        ParamSource::Body => resolve_body_param(py, ctx, python_type),
        ParamSource::RawBody => resolve_raw_body(py, ctx),
        ParamSource::RawRequest => resolve_raw_request(py, ctx),
    }
}

/// Resolve a path parameter.
fn resolve_path_param<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let value = ctx
        .path_params
        .iter()
        .find(|(k, _)| k == &param.name)
        .map(|(_, v)| v.as_str());

    match value {
        Some(v) => convert_path_value(py, v, param.type_qualname.as_str()),
        None if !param.required => Ok(py.None().into_bound(py)),
        None => Err(AppError::BadRequest(format!(
            "missing path parameter: {}",
            param.name
        ))),
    }
}

/// Convert a path parameter string to the target Python type.
fn convert_path_value<'py>(
    py: Python<'py>,
    value: &str,
    type_name: &str,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let py_str = PyString::new(py, value);

    match type_name {
        "int" => {
            let builtins = py
                .import(c"builtins")
                .map_err(|e| AppError::Internal(e.to_string()))?;
            builtins
                .getattr(c"int")
                .and_then(|f| f.call1((py_str,)))
                .map_err(|_| {
                    AppError::BadRequest(format!("path param is not a valid integer: {value}"))
                })
        }
        "float" => {
            let builtins = py
                .import(c"builtins")
                .map_err(|e| AppError::Internal(e.to_string()))?;
            builtins
                .getattr(c"float")
                .and_then(|f| f.call1((py_str,)))
                .map_err(|_| {
                    AppError::BadRequest(format!("path param is not a valid float: {value}"))
                })
        }
        _ => Ok(py_str.into_any()),
    }
}

/// Resolve a query parameter.
fn resolve_query_param<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    // Take last value for scalar types (standard browser behavior for duplicate keys).
    let value = ctx
        .query_params
        .iter()
        .rev()
        .find(|(k, _)| k == &param.name)
        .map(|(_, v)| v.as_str());

    match value {
        Some(v) => convert_path_value(py, v, param.type_qualname.as_str()),
        None if !param.required => Ok(py.None().into_bound(py)),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["query".to_owned(), param.name.clone()],
            msg: "Field required".to_owned(),
            r#type: "missing".to_owned(),
        }])),
    }
}

/// Resolve a Body parameter via Pydantic `model_validate_json()`.
fn resolve_body_param<'py>(
    py: Python<'py>,
    ctx: &RequestContext,
    python_type: Option<&Py<PyAny>>,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let body = ctx
        .body
        .as_ref()
        .ok_or_else(|| AppError::Internal("body not read for Body param".to_owned()))?;

    let model_cls = python_type
        .ok_or_else(|| AppError::Internal("missing python_type for Body param".to_owned()))?;

    model_cls
        .bind(py)
        .call_method1(c"model_validate_json", (body.as_ref(),))
        .map_err(|e| {
            let errors = extract_pydantic_errors(py, &e);
            if errors.is_empty() {
                AppError::BadRequest(format!("body validation failed: {e}"))
            } else {
                AppError::Validation(errors)
            }
        })
}

/// Extract structured errors from a Pydantic `ValidationError`.
fn extract_pydantic_errors(py: Python<'_>, err: &pyo3::PyErr) -> Vec<ValidationErrorItem> {
    let err_value = err.value(py);
    let Ok(errors_list) = err_value.call_method0(c"errors") else {
        return Vec::new();
    };
    let Ok(iter) = errors_list.try_iter() else {
        return Vec::new();
    };
    iter.filter_map(|item| {
        let item = item.ok()?;
        let loc = item
            .get_item(c"loc")
            .ok()?
            .try_iter()
            .ok()?
            .filter_map(|l| l.ok()?.str().ok().map(|s| s.to_string()))
            .collect();
        let msg = item.get_item(c"msg").ok()?.str().ok()?.to_string();
        let r#type = item.get_item(c"type").ok()?.str().ok()?.to_string();
        Some(ValidationErrorItem { loc, msg, r#type })
    })
    .collect()
}

/// Resolve raw body bytes.
fn resolve_raw_body<'py>(
    py: Python<'py>,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let body = ctx.body.as_ref().map(Bytes::as_ref).unwrap_or_default();
    Ok(PyBytes::new(py, body).into_any())
}

/// Resolve raw request object — constructs the Rust-backed `Request` directly.
fn resolve_raw_request<'py>(
    py: Python<'py>,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let headers_dict = PyDict::new(py);
    for (name, value) in &ctx.headers {
        let _ = headers_dict.set_item(name.as_str(), value.to_str().unwrap_or(""));
    }

    let body = ctx.body.as_ref().map(Bytes::as_ref).unwrap_or_default();

    let request = crate::pyapi::Request {
        method: String::new(),
        path: String::new(),
        query_string: String::new(),
        headers: headers_dict.clone().into_any().unbind(),
        cookies: PyDict::new(py).into_any().unbind(),
        body_bytes: PyBytes::new(py, body).into_any().unbind(),
    };

    Py::new(py, request)
        .map(|obj| obj.into_bound(py).into_any())
        .map_err(|e| AppError::Internal(format!("construct Request: {e}")))
}

// ── Step 3: Serialize response ──────────────────────────────────────────

/// Validate return type and serialize the Python result to an HTTP response.
fn serialize_result(
    py: Python<'_>,
    result: &Py<PyAny>,
    route: &BoundRoute,
) -> Result<Response, AppError> {
    let result_ref = result.bind(py);
    validate_return_type(py, result_ref, route)?;
    serialize_response(py, result_ref, route)
}

/// Verify the handler returned an instance of the declared response model.
fn validate_return_type(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    route: &BoundRoute,
) -> Result<(), AppError> {
    let Some(model) = route.response_model.as_ref() else {
        return Ok(());
    };
    if result.is_instance(model.bind(py)).unwrap_or(false) {
        return Ok(());
    }
    let actual = result
        .get_type()
        .name()
        .map_or_else(|_| "unknown".to_owned(), |n| n.to_string());
    Err(AppError::Internal(format!(
        "handler returned {actual}, expected {}",
        route.manifest.response_type
    )))
}

/// Serialize the Python result to an HTTP response.
fn serialize_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    route: &BoundRoute,
) -> Result<Response, AppError> {
    match &route.manifest.response_type {
        ResponseType::Model { .. } => serialize_model_response(py, result),
        // TODO(phase-7): streaming serialization via AsgiSend
        ResponseType::StreamingResponse => Err(AppError::Internal(
            "streaming responses not yet implemented".to_owned(),
        )),
        ResponseType::RawResponse => serialize_raw_response(py, result),
    }
}

/// Serialize a `ResponseModel` via `model_dump_json(by_alias=True)`.
fn serialize_model_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
) -> Result<Response, AppError> {
    let by_alias = PyDict::new(py);
    let _ = by_alias.set_item(c"by_alias", true);

    let json_bytes: Vec<u8> = result
        .call_method(c"model_dump_json", (), Some(&by_alias))
        .and_then(|s| s.extract())
        .map_err(|e| AppError::Internal(format!("model_dump_json: {e}")))?;

    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(json_bytes))
        .map_err(|e| AppError::Internal(format!("build response: {e}")))
}

/// Serialize a raw `Response` object.
fn serialize_raw_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
) -> Result<Response, AppError> {
    let status: u16 = result
        .getattr(c"status")
        .and_then(|s| s.extract())
        .unwrap_or(200);

    let json_body = result
        .getattr(c"body")
        .ok()
        .map(|b| {
            let json_mod = py
                .import(c"json")
                .map_err(|e| AppError::Internal(format!("import json: {e}")))?;
            let dumped: String = json_mod
                .call_method1(c"dumps", (&b,))
                .and_then(|s| s.extract())
                .map_err(|e| AppError::Internal(format!("json.dumps: {e}")))?;
            Ok::<_, AppError>(dumped.into_bytes())
        })
        .transpose()?
        .unwrap_or_default();

    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    Response::builder()
        .status(status_code)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(json_body))
        .map_err(|e| AppError::Internal(format!("build response: {e}")))
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Convert a Python exception to an [`AppError`].
///
/// Uses `is_instance_of` against the Rust-defined `create_exception!` types —
/// no runtime Python import needed.
fn python_err_to_app_error(err: pyo3::PyErr) -> AppError {
    Python::attach(|py| {
        if err.is_instance_of::<crate::pyapi::NotFound>(py) {
            return extract_detail(err.value(py), AppError::NotFound);
        }
        if err.is_instance_of::<crate::pyapi::BadRequest>(py) {
            return extract_detail(err.value(py), AppError::BadRequest);
        }
        if err.is_instance_of::<crate::pyapi::Forbidden>(py) {
            return extract_detail(err.value(py), AppError::Forbidden);
        }

        AppError::Internal(format!("{err}"))
    })
}

/// Extract the detail message from a framework exception.
///
/// Framework exceptions set `args[0]` as the detail message (standard
/// Python Exception pattern). Falls back to `str(value)`.
fn extract_detail(value: &pyo3::Bound<'_, PyAny>, f: fn(String) -> AppError) -> AppError {
    let detail = value.str().map(|s| s.to_string()).unwrap_or_default();
    f(detail)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn make_parts(uri: &str) -> http::request::Parts {
        http::Request::builder()
            .uri(uri)
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    #[test]
    fn extract_query_params_empty() {
        let parts = make_parts("/items");
        let params = extract_query_params(&parts);
        assert!(params.is_empty());
    }

    #[test]
    fn extract_query_params_single() {
        let parts = make_parts("/items?page=1");
        let params = extract_query_params(&parts);
        assert_eq!(params, vec![("page".to_owned(), "1".to_owned())]);
    }

    #[test]
    fn extract_query_params_multiple() {
        let parts = make_parts("/items?page=1&sort=name");
        let params = extract_query_params(&parts);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], ("page".to_owned(), "1".to_owned()));
        assert_eq!(params[1], ("sort".to_owned(), "name".to_owned()));
    }

    #[test]
    fn extract_query_params_percent_encoded() {
        let parts = make_parts("/search?q=hello%20world");
        let params = extract_query_params(&parts);
        assert_eq!(params, vec![("q".to_owned(), "hello world".to_owned())]);
    }

    #[test]
    fn extract_query_params_plus_as_space() {
        let parts = make_parts("/search?q=hello+world");
        let params = extract_query_params(&parts);
        assert_eq!(params, vec![("q".to_owned(), "hello world".to_owned())]);
    }

    #[test]
    fn extract_query_params_duplicate_keys() {
        let parts = make_parts("/items?tag=a&tag=b");
        let params = extract_query_params(&parts);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].0, "tag");
        assert_eq!(params[1].0, "tag");
    }

    #[test]
    fn app_state_debug() {
        let state = AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
        };
        let dbg = format!("{state:?}");
        assert!(dbg.contains("AppState"));
        assert!(dbg.contains("max_body_limit"));
    }
}
