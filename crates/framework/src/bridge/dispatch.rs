//! Handler dispatch trait and request-response implementation.
//!
//! Each [`HandlerKind`](crate::route::HandlerKind) gets its own
//! [`HandlerDispatch`] impl, owning the full request lifecycle.

use super::context::RequestContext;
use crate::error::{AppError, BodyParseKind, ValidationErrorItem};
use crate::event_loop::EventLoopHandle;
use crate::route::{BoundRoute, Model, ParamSource, ResponseType};
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
use bytes::Bytes;
use http::StatusCode;
use pyo3::types::{PyAnyMethods, PyBytes, PyDict, PyDictMethods, PyTypeMethods};
use pyo3::{Py, PyAny, Python};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Lifecycle-scoped state shared across all routes in a single worker.
#[derive(Clone)]
pub struct AppState {
    /// Max request body size in bytes.
    pub max_body_limit: crate::route::BodyLimit,
    /// Handle to the persistent asyncio event loop.
    pub loop_handle: EventLoopHandle,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("max_body_limit", &self.max_body_limit)
            .field("loop_handle", &self.loop_handle)
            .finish_non_exhaustive()
    }
}

/// Handles the full request lifecycle for a specific handler kind.
///
/// Implementations work entirely on transport-neutral types. The axum
/// boundary lives in `bridge/mod.rs::python_handler` only.
pub trait HandlerDispatch: Send + Sync + std::fmt::Debug {
    /// Process a request and return a transport-neutral response.
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>>;
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
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>> {
        Box::pin(async move {
            let ctx = extract_context(&mut request, &route, &app_state).await?;
            let result = invoke_handler(&route, &ctx, &app_state.loop_handle).await?;
            Python::attach(|py| serialize_result(py, &result, &route))
        })
    }
}

// ── Step 1: Extract HTTP parts ──────────────────────────────────────────

/// Extract HTTP parts into [`RequestContext`] from an [`InboundRequest`].
///
/// Path params come from `request.path_params`, query params are parsed
/// from the raw query string via `form_urlencoded`.
pub(super) async fn extract_context(
    request: &mut InboundRequest,
    route: &BoundRoute,
    app_state: &AppState,
) -> Result<RequestContext, AppError> {
    let query_params: Vec<(String, String)> = form_urlencoded::parse(&request.query_string)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let body = if route.has_body_param {
        let body_stream = request.take_body();
        let bytes = body_stream
            .collect(app_state.max_body_limit.0)
            .await
            .map_err(|_| AppError::BodyParse(BodyParseKind::BodyTooLarge))?;
        Some(bytes)
    } else {
        None
    };

    Ok(RequestContext {
        path_params: request.path_params.clone(),
        query_params,
        headers: request.headers.clone(),
        body,
    })
}

// ── Step 2: Invoke handler ──────────────────────────────────────────────

/// Call the Python handler, branching on async vs sync.
///
/// Async handlers: build kwargs, get coroutine, drive on the persistent event loop.
/// Sync handlers: build kwargs, run the call in `spawn_blocking`.
async fn invoke_handler(
    route: &BoundRoute,
    ctx: &RequestContext,
    loop_handle: &EventLoopHandle,
) -> Result<Py<PyAny>, AppError> {
    tracing::debug!(
        handler = %route.manifest.handler_qualname,
        params = route.params.len(),
        is_async = route.manifest.is_async_handler,
        "invoke_handler: calling handler"
    );

    let result = if route.manifest.is_async_handler {
        invoke_handler_async(route, ctx, loop_handle).await?
    } else {
        invoke_handler_sync(route, ctx).await?
    };

    tracing::debug!("invoke_handler: handler completed successfully");
    Ok(result)
}

/// Async path: build kwargs, obtain the coroutine, drive it on the event loop.
async fn invoke_handler_async(
    route: &BoundRoute,
    ctx: &RequestContext,
    loop_handle: &EventLoopHandle,
) -> Result<Py<PyAny>, AppError> {
    let coro = Python::attach(|py| {
        let kwargs = build_kwargs(py, route, ctx)?;
        route
            .handler
            .call(py, &kwargs)
            .map(|b| b.unbind())
            .map_err(|e| AppError::Internal(format!("handler call failed: {e}")))
    })?;
    loop_handle.drive_coroutine(coro).await
}

/// Sync path: build kwargs, run the handler on the blocking threadpool.
async fn invoke_handler_sync(
    route: &BoundRoute,
    ctx: &RequestContext,
) -> Result<Py<PyAny>, AppError> {
    let (handler, kwargs) = Python::attach(|py| {
        let kwargs = build_kwargs(py, route, ctx)?;
        Ok::<_, AppError>((route.handler.clone_ref(py), kwargs.unbind()))
    })?;
    tokio::task::spawn_blocking(move || {
        Python::attach(|py| {
            handler
                .call(py, kwargs.bind(py))
                .map(|b| b.unbind())
                .map_err(|e| AppError::Internal(format!("handler call failed: {e}")))
        })
    })
    .await
    .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?
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
    python_type: Option<&Model>,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    match param.source {
        ParamSource::Path => super::extract::extract_path_value(
            py,
            &ctx.path_params,
            &param.name,
            param.type_qualname.as_str(),
            param.required,
        ),
        ParamSource::Query => super::extract::extract_query_value(
            py,
            &ctx.query_params,
            &param.name,
            param.type_qualname.as_str(),
            param.required,
            param.default_json.as_ref(),
        ),
        ParamSource::Header => {
            let wire_name = param.alias.as_deref().unwrap_or(&param.name);
            super::extract::extract_header_value(
                py,
                &ctx.headers,
                wire_name,
                param.type_qualname.as_str(),
                param.required,
            )
        }
        ParamSource::Cookie => super::extract::extract_cookie_value(
            py,
            &ctx.headers,
            &param.name,
            param.type_qualname.as_str(),
            param.required,
        ),
        ParamSource::Body => resolve_body_param(py, ctx, python_type),
        ParamSource::RawBody => resolve_raw_body(py, ctx),
    }
}

/// Resolve a Body parameter via Pydantic `model_validate_json()`.
fn resolve_body_param<'py>(
    py: Python<'py>,
    ctx: &RequestContext,
    python_type: Option<&Model>,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let body = ctx
        .body
        .as_ref()
        .ok_or_else(|| AppError::Internal("body not read for Body param".to_owned()))?;

    let model = python_type
        .ok_or_else(|| AppError::Internal("missing python_type for Body param".to_owned()))?;

    model.validate_json(py, body.as_ref()).map_err(|e| {
        let errors = extract_pydantic_errors(py, &e);
        if errors.is_empty() {
            AppError::BadRequest(format!("body validation failed: {e}"))
        } else {
            AppError::Validation(errors)
        }
    })
}

/// Extract structured errors from a Pydantic `ValidationError`.
pub(super) fn extract_pydantic_errors(
    py: Python<'_>,
    err: &pyo3::PyErr,
) -> Vec<ValidationErrorItem> {
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

// ── Step 3: Serialize response ──────────────────────────────────────────

/// Validate return type and serialize the Python result to an outbound response.
pub(super) fn serialize_result(
    py: Python<'_>,
    result: &Py<PyAny>,
    route: &BoundRoute,
) -> Result<OutboundResponse, AppError> {
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
    if model.is_instance(py, result) {
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

/// Serialize the Python result to an outbound response.
fn serialize_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    route: &BoundRoute,
) -> Result<OutboundResponse, AppError> {
    match &route.manifest.response_type {
        ResponseType::Model { .. } => {
            serialize_model_response(py, result, route.manifest.status_code)
        }
        // TODO(phase-7): streaming serialization via AsgiSend
        ResponseType::StreamingResponse => Err(AppError::Internal(
            "streaming responses not yet implemented".to_owned(),
        )),
        ResponseType::RawResponse => {
            serialize_untyped_response(py, result, route.manifest.status_code)
        }
    }
}

/// Serialize a `ResponseModel` via `model_dump_json(by_alias=True)`.
fn serialize_model_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    status_code: u16,
) -> Result<OutboundResponse, AppError> {
    let by_alias = PyDict::new(py);
    let _ = by_alias.set_item(c"by_alias", true);

    let json_bytes: Vec<u8> = result
        .call_method(c"model_dump_json", (), Some(&by_alias))
        .and_then(|s| s.extract())
        .map_err(|e| AppError::Internal(format!("model_dump_json: {e}")))?;

    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );

    Ok(OutboundResponse {
        status,
        headers,
        body: ResponseBody::Fixed(Bytes::from(json_bytes)),
    })
}

/// Serialize a handler result that has no declared `response_model`.
///
/// Tries Pydantic `model_dump_json` first (handler returned a model without
/// declaring `response_model=`), then falls back to `json.dumps` for
/// dicts, lists, and primitives.
fn serialize_untyped_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    status_code: u16,
) -> Result<OutboundResponse, AppError> {
    let json_bytes = try_pydantic_dump(py, result)
        .or_else(|| try_json_dumps(py, result))
        .ok_or_else(|| AppError::Internal("failed to serialize handler return value".to_owned()))?;

    let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );

    Ok(OutboundResponse {
        status,
        headers,
        body: ResponseBody::Fixed(Bytes::from(json_bytes)),
    })
}

/// Try serializing via Pydantic `model_dump_json(by_alias=True)`.
fn try_pydantic_dump(py: Python<'_>, result: &pyo3::Bound<'_, PyAny>) -> Option<Vec<u8>> {
    let kwargs = PyDict::new(py);
    let _ = kwargs.set_item(c"by_alias", true);
    result
        .call_method(c"model_dump_json", (), Some(&kwargs))
        .ok()
        .and_then(|s| s.extract::<Vec<u8>>().ok())
}

/// Try serializing via `json.dumps`.
fn try_json_dumps(py: Python<'_>, result: &pyo3::Bound<'_, PyAny>) -> Option<Vec<u8>> {
    let json_mod = py.import(c"json").ok()?;
    let dumped: String = json_mod
        .call_method1(c"dumps", (result,))
        .ok()?
        .extract()
        .ok()?;
    Some(dumped.into_bytes())
}

// ── Helpers ─────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::with_py;

    /// Parse a raw query string into key-value pairs (mirrors extract_context logic).
    fn parse_query_string(query: &[u8]) -> Vec<(String, String)> {
        form_urlencoded::parse(query)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn parse_query_string_empty() {
        let params = parse_query_string(b"");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_query_string_single() {
        let params = parse_query_string(b"page=1");
        assert_eq!(params, vec![("page".to_owned(), "1".to_owned())]);
    }

    #[test]
    fn parse_query_string_multiple() {
        let params = parse_query_string(b"page=1&sort=name");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], ("page".to_owned(), "1".to_owned()));
        assert_eq!(params[1], ("sort".to_owned(), "name".to_owned()));
    }

    #[test]
    fn parse_query_string_percent_encoded() {
        let params = parse_query_string(b"q=hello%20world");
        assert_eq!(params, vec![("q".to_owned(), "hello world".to_owned())]);
    }

    #[test]
    fn parse_query_string_plus_as_space() {
        let params = parse_query_string(b"q=hello+world");
        assert_eq!(params, vec![("q".to_owned(), "hello world".to_owned())]);
    }

    #[test]
    fn parse_query_string_duplicate_keys() {
        let params = parse_query_string(b"tag=a&tag=b");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].0, "tag");
        assert_eq!(params[1].0, "tag");
    }

    #[test]
    fn app_state_debug() {
        let mut event_loop = crate::event_loop::EventLoop::start().unwrap();
        let state = AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
            loop_handle: event_loop.handle(),
        };
        let dbg = format!("{state:?}");
        assert!(dbg.contains("AppState"));
        assert!(dbg.contains("max_body_limit"));
        event_loop.stop();
    }

    // ── Header extraction ───────────────────────────────────────────────

    use crate::bridge::extract;

    fn make_headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        for (k, v) in pairs {
            headers.insert(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                http::HeaderValue::from_str(v).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn extract_header_present() {
        with_py(|py| {
            let headers = make_headers(&[("x-token", "abc")]);
            let result = extract::extract_header_value(py, &headers, "x-token", "str", true);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "abc");
        });
    }

    #[test]
    fn extract_header_alias() {
        with_py(|py| {
            let headers = make_headers(&[("x-token", "xyz")]);
            let result = extract::extract_header_value(py, &headers, "x-token", "str", true);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "xyz");
        });
    }

    #[test]
    fn extract_header_int_conversion() {
        with_py(|py| {
            let headers = make_headers(&[("x-count", "42")]);
            let result = extract::extract_header_value(py, &headers, "x-count", "int", true);
            assert!(result.is_ok());
            let val: i64 = result.unwrap().extract().unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn extract_header_missing_required() {
        with_py(|py| {
            let headers = make_headers(&[]);
            let result = extract::extract_header_value(py, &headers, "x-token", "str", true);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, AppError::Validation(_)));
        });
    }

    #[test]
    fn extract_header_missing_optional() {
        with_py(|py| {
            let headers = make_headers(&[]);
            let result = extract::extract_header_value(py, &headers, "x-token", "str", false);
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        });
    }

    // ── Cookie extraction ───────────────────────────────────────────────

    #[test]
    fn extract_cookie_present() {
        with_py(|py| {
            let headers = make_headers(&[("cookie", "session=abc123; theme=dark")]);
            let result = extract::extract_cookie_value(py, &headers, "session", "str", true);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "abc123");
        });
    }

    #[test]
    fn extract_cookie_missing_required() {
        with_py(|py| {
            let headers = make_headers(&[]);
            let result = extract::extract_cookie_value(py, &headers, "session", "str", true);
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
        });
    }

    #[test]
    fn extract_cookie_missing_optional() {
        with_py(|py| {
            let headers = make_headers(&[]);
            let result = extract::extract_cookie_value(py, &headers, "session", "str", false);
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        });
    }

    #[test]
    fn extract_cookie_multiple_cookies() {
        with_py(|py| {
            let headers = make_headers(&[("cookie", "a=1; b=2; c=3")]);
            let result = extract::extract_cookie_value(py, &headers, "b", "str", true);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "2");
        });
    }

    // ── convert_scalar ───────────────────────────────────────────────────

    #[test]
    fn convert_scalar_float() {
        with_py(|py| {
            let result = extract::convert_scalar(py, "2.5", "float").unwrap();
            let val: f64 = result.extract().unwrap();
            assert!((val - 2.5).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn convert_scalar_invalid_float() {
        with_py(|py| {
            let result = extract::convert_scalar(py, "not_a_number", "float");
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), AppError::BadRequest(_)));
        });
    }

    #[test]
    fn convert_scalar_unknown_type_returns_string() {
        with_py(|py| {
            let result = extract::convert_scalar(py, "hello", "uuid").unwrap();
            let val: String = result.extract().unwrap();
            assert_eq!(val, "hello");
        });
    }

    // ── resolve_raw_body ───────────────────────────────────────────────

    #[test]
    fn resolve_raw_body_with_body() {
        with_py(|py| {
            let ctx = RequestContext {
                path_params: Vec::new(),
                query_params: Vec::new(),
                headers: http::HeaderMap::new(),
                body: Some(Bytes::from("raw bytes")),
            };
            let result = resolve_raw_body(py, &ctx).unwrap();
            let val: Vec<u8> = result.extract().unwrap();
            assert_eq!(val, b"raw bytes");
        });
    }

    #[test]
    fn resolve_raw_body_no_body() {
        with_py(|py| {
            let ctx = RequestContext {
                path_params: Vec::new(),
                query_params: Vec::new(),
                headers: http::HeaderMap::new(),
                body: None,
            };
            let result = resolve_raw_body(py, &ctx).unwrap();
            let val: Vec<u8> = result.extract().unwrap();
            assert!(val.is_empty());
        });
    }
}
