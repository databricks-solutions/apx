//! Handler dispatch trait and request-response implementation.
//!
//! Each [`HandlerKind`](crate::route::HandlerKind) gets its own
//! [`HandlerDispatch`] impl, owning the full request lifecycle.

use super::context::RequestContext;
use crate::error::{AppError, BodyParseKind, ValidationErrorItem};
use crate::route::{BoundRoute, ParamSource, ResponseType};
use crate::transport::types::{InboundRequest, OutboundResponse, ResponseBody};
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
            let result = invoke_handler(&route, &ctx).await?;
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
        ParamSource::Header => resolve_header_param(py, param, ctx),
        ParamSource::Cookie => resolve_cookie_param(py, param, ctx),
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
pub(super) fn convert_path_value<'py>(
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

/// Resolve a header parameter by looking up the wire name in the header map.
fn resolve_header_param<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let wire_name = param.alias.as_deref().unwrap_or(&param.name);
    match ctx.headers.get(wire_name) {
        Some(value) => {
            let value_str = value.to_str().map_err(|_| {
                AppError::BadRequest(format!(
                    "header '{wire_name}' contains non-ASCII characters"
                ))
            })?;
            convert_path_value(py, value_str, param.type_qualname.as_str())
        }
        None if !param.required => Ok(py.None().into_bound(py)),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["header".into(), wire_name.into()],
            msg: format!("missing required header: {wire_name}"),
            r#type: "missing".into(),
        }])),
    }
}

/// Resolve a cookie parameter by parsing the `Cookie` header.
fn resolve_cookie_param<'py>(
    py: Python<'py>,
    param: &crate::route::ParamManifest,
    ctx: &RequestContext,
) -> Result<pyo3::Bound<'py, PyAny>, AppError> {
    let cookie_header = ctx
        .headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((name, value)) = pair.split_once('=')
            && name.trim() == param.name
        {
            return convert_path_value(py, value.trim(), param.type_qualname.as_str());
        }
    }

    if param.required {
        Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["cookie".into(), param.name.clone()],
            msg: format!("missing required cookie: {}", param.name),
            r#type: "missing".into(),
        }]))
    } else {
        Ok(py.None().into_bound(py))
    }
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

/// Serialize the Python result to an outbound response.
fn serialize_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
    route: &BoundRoute,
) -> Result<OutboundResponse, AppError> {
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
) -> Result<OutboundResponse, AppError> {
    let by_alias = PyDict::new(py);
    let _ = by_alias.set_item(c"by_alias", true);

    let json_bytes: Vec<u8> = result
        .call_method(c"model_dump_json", (), Some(&by_alias))
        .and_then(|s| s.extract())
        .map_err(|e| AppError::Internal(format!("model_dump_json: {e}")))?;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        "application/json"
            .parse()
            .map_err(|e| AppError::Internal(format!("header value: {e}")))?,
    );

    Ok(OutboundResponse {
        status: StatusCode::OK,
        headers,
        body: ResponseBody::Fixed(Bytes::from(json_bytes)),
    })
}

/// Serialize a raw `Response` object.
fn serialize_raw_response(
    py: Python<'_>,
    result: &pyo3::Bound<'_, PyAny>,
) -> Result<OutboundResponse, AppError> {
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

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        "application/json"
            .parse()
            .map_err(|e| AppError::Internal(format!("header value: {e}")))?,
    );

    Ok(OutboundResponse {
        status: status_code,
        headers,
        body: ResponseBody::Fixed(Bytes::from(json_body)),
    })
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
        let state = AppState {
            max_body_limit: crate::route::BodyLimit::DEFAULT,
        };
        let dbg = format!("{state:?}");
        assert!(dbg.contains("AppState"));
        assert!(dbg.contains("max_body_limit"));
    }

    // ── Header extraction ───────────────────────────────────────────────

    fn make_header_param(
        name: &str,
        alias: Option<&str>,
        type_name: &str,
        required: bool,
    ) -> crate::route::ParamManifest {
        crate::route::ParamManifest {
            name: name.to_owned(),
            source: ParamSource::Header,
            type_qualname: crate::route::QualName::new(type_name).unwrap(),
            required,
            alias: alias.map(str::to_owned),
            json_schema: None,
            default_json: None,
        }
    }

    fn make_cookie_param(
        name: &str,
        type_name: &str,
        required: bool,
    ) -> crate::route::ParamManifest {
        crate::route::ParamManifest {
            name: name.to_owned(),
            source: ParamSource::Cookie,
            type_qualname: crate::route::QualName::new(type_name).unwrap(),
            required,
            alias: None,
            json_schema: None,
            default_json: None,
        }
    }

    fn ctx_with_headers(pairs: &[(&str, &str)]) -> RequestContext {
        let mut headers = http::HeaderMap::new();
        for (k, v) in pairs {
            headers.insert(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                http::HeaderValue::from_str(v).unwrap(),
            );
        }
        RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers,
            body: None,
        }
    }

    #[test]
    fn resolve_header_param_present() {
        with_py(|py| {
            let param = make_header_param("x-token", None, "str", true);
            let ctx = ctx_with_headers(&[("x-token", "abc")]);
            let result = resolve_header_param(py, &param, &ctx);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "abc");
        });
    }

    #[test]
    fn resolve_header_param_alias() {
        with_py(|py| {
            let param = make_header_param("token", Some("x-token"), "str", true);
            let ctx = ctx_with_headers(&[("x-token", "xyz")]);
            let result = resolve_header_param(py, &param, &ctx);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "xyz");
        });
    }

    #[test]
    fn resolve_header_param_int_conversion() {
        with_py(|py| {
            let param = make_header_param("x-count", None, "int", true);
            let ctx = ctx_with_headers(&[("x-count", "42")]);
            let result = resolve_header_param(py, &param, &ctx);
            assert!(result.is_ok());
            let val: i64 = result.unwrap().extract().unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn resolve_header_param_missing_required() {
        with_py(|py| {
            let param = make_header_param("x-token", None, "str", true);
            let ctx = ctx_with_headers(&[]);
            let result = resolve_header_param(py, &param, &ctx);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, AppError::Validation(_)));
        });
    }

    #[test]
    fn resolve_header_param_missing_optional() {
        with_py(|py| {
            let param = make_header_param("x-token", None, "str", false);
            let ctx = ctx_with_headers(&[]);
            let result = resolve_header_param(py, &param, &ctx);
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        });
    }

    // ── Cookie extraction ───────────────────────────────────────────────

    #[test]
    fn resolve_cookie_param_present() {
        with_py(|py| {
            let param = make_cookie_param("session", "str", true);
            let ctx = ctx_with_headers(&[("cookie", "session=abc123; theme=dark")]);
            let result = resolve_cookie_param(py, &param, &ctx);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "abc123");
        });
    }

    #[test]
    fn resolve_cookie_param_missing_required() {
        with_py(|py| {
            let param = make_cookie_param("session", "str", true);
            let ctx = ctx_with_headers(&[]);
            let result = resolve_cookie_param(py, &param, &ctx);
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
        });
    }

    #[test]
    fn resolve_cookie_param_missing_optional() {
        with_py(|py| {
            let param = make_cookie_param("session", "str", false);
            let ctx = ctx_with_headers(&[]);
            let result = resolve_cookie_param(py, &param, &ctx);
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        });
    }

    #[test]
    fn resolve_cookie_param_multiple_cookies() {
        with_py(|py| {
            let param = make_cookie_param("b", "str", true);
            let ctx = ctx_with_headers(&[("cookie", "a=1; b=2; c=3")]);
            let result = resolve_cookie_param(py, &param, &ctx);
            assert!(result.is_ok());
            let val: String = result.unwrap().extract().unwrap();
            assert_eq!(val, "2");
        });
    }

    // ── convert_path_value ─────────────────────────────────────────────

    #[test]
    fn convert_path_value_float() {
        with_py(|py| {
            let result = convert_path_value(py, "2.5", "float").unwrap();
            let val: f64 = result.extract().unwrap();
            assert!((val - 2.5).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn convert_path_value_invalid_float() {
        with_py(|py| {
            let result = convert_path_value(py, "not_a_number", "float");
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), AppError::BadRequest(_)));
        });
    }

    #[test]
    fn convert_path_value_unknown_type_returns_string() {
        with_py(|py| {
            let result = convert_path_value(py, "hello", "uuid").unwrap();
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

    // ── resolve_raw_request ────────────────────────────────────────────

    #[test]
    fn resolve_raw_request_with_headers_and_body() {
        with_py(|py| {
            let mut headers = http::HeaderMap::new();
            headers.insert("x-token", "secret".parse().unwrap());
            let ctx = RequestContext {
                path_params: Vec::new(),
                query_params: Vec::new(),
                headers,
                body: Some(Bytes::from("body data")),
            };
            let result = resolve_raw_request(py, &ctx).unwrap();
            assert!(result.is_instance_of::<crate::pyapi::Request>());
        });
    }

    #[test]
    fn resolve_raw_request_empty() {
        with_py(|py| {
            let ctx = RequestContext {
                path_params: Vec::new(),
                query_params: Vec::new(),
                headers: http::HeaderMap::new(),
                body: None,
            };
            let result = resolve_raw_request(py, &ctx).unwrap();
            assert!(result.is_instance_of::<crate::pyapi::Request>());
        });
    }

    // ── python_err_to_app_error ────────────────────────────────────────

    #[test]
    fn python_err_to_app_error_not_found() {
        with_py(|_py| {
            let err = pyo3::PyErr::new::<crate::pyapi::NotFound, _>("item not found");
            let app_err = python_err_to_app_error(err);
            assert!(matches!(app_err, AppError::NotFound(_)));
        });
    }

    #[test]
    fn python_err_to_app_error_bad_request() {
        with_py(|_py| {
            let err = pyo3::PyErr::new::<crate::pyapi::BadRequest, _>("invalid input");
            let app_err = python_err_to_app_error(err);
            assert!(matches!(app_err, AppError::BadRequest(_)));
        });
    }

    #[test]
    fn python_err_to_app_error_forbidden() {
        with_py(|_py| {
            let err = pyo3::PyErr::new::<crate::pyapi::Forbidden, _>("access denied");
            let app_err = python_err_to_app_error(err);
            assert!(matches!(app_err, AppError::Forbidden(_)));
        });
    }

    #[test]
    fn python_err_to_app_error_generic() {
        with_py(|_py| {
            let err = pyo3::PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("something broke");
            let app_err = python_err_to_app_error(err);
            assert!(matches!(app_err, AppError::Internal(_)));
        });
    }
}
