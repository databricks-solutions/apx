//! Compiled dependency plan executor dispatch.
//!
//! Executes pre-compiled [`DependencyPlan`] steps to resolve handler kwargs,
//! then calls the handler directly. No FastAPI dependency solving at runtime.

use super::context::RequestContext;
use super::dispatch::{
    AppState, HandlerDispatch, convert_path_value, extract_context, extract_pydantic_errors,
    serialize_result,
};
use crate::discovery::bind::import_qualified_name;
use crate::error::{AppError, ValidationErrorItem};
use crate::route::{BoundRoute, DependencyPlan, DependencyStep};
use crate::runtime::lifecycle::LifecycleCache;
use crate::transport::types::{InboundRequest, OutboundResponse};
use pyo3::conversion::IntoPyObject;
use pyo3::types::{PyAnyMethods, PyDictMethods, PyString};
use pyo3::{Py, PyAny, Python};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Dispatch via compiled dependency plan execution.
///
/// Executes topologically sorted [`DependencyStep`]s to build handler kwargs,
/// then calls the handler directly without FastAPI's `solve_dependencies`.
#[derive(Debug)]
pub struct PlanExecutorDispatch {
    lifecycle_cache: Arc<LifecycleCache>,
}

impl PlanExecutorDispatch {
    /// Create a new plan executor with a shared lifecycle cache.
    pub fn new(lifecycle_cache: Arc<LifecycleCache>) -> Self {
        Self { lifecycle_cache }
    }
}

impl HandlerDispatch for PlanExecutorDispatch {
    fn handle(
        &self,
        route: Arc<BoundRoute>,
        app_state: Arc<AppState>,
        mut request: InboundRequest,
    ) -> Pin<Box<dyn Future<Output = Result<OutboundResponse, AppError>> + Send>> {
        let cache = Arc::clone(&self.lifecycle_cache);
        Box::pin(async move {
            let ctx = extract_context(&mut request, &route, &app_state).await?;

            let plan = route
                .manifest
                .dependency_plan
                .as_ref()
                .ok_or_else(|| AppError::Internal("route has no dependency plan".to_owned()))?;

            let kwargs = execute_plan(plan, &ctx, &cache).await?;
            let result = invoke_with_kwargs(&route, &kwargs).await?;
            Python::attach(|py| serialize_result(py, &result, &route))
        })
    }
}

/// Execute all steps in a dependency plan, producing resolved values.
async fn execute_plan(
    plan: &DependencyPlan,
    ctx: &RequestContext,
    cache: &LifecycleCache,
) -> Result<HashMap<String, Py<PyAny>>, AppError> {
    let mut resolved: HashMap<String, Py<PyAny>> = HashMap::with_capacity(plan.steps.len());

    for step in &plan.steps {
        execute_step(step, ctx, cache, &mut resolved).await?;
    }

    Ok(resolved)
}

/// Execute a single dependency step, inserting the result into `resolved`.
async fn execute_step(
    step: &DependencyStep,
    ctx: &RequestContext,
    cache: &LifecycleCache,
    resolved: &mut HashMap<String, Py<PyAny>>,
) -> Result<(), AppError> {
    match step {
        DependencyStep::ExtractPath {
            name,
            type_qualname,
        } => {
            let value = extract_path_step(ctx, name, type_qualname.as_str())?;
            resolved.insert(name.clone(), value);
        }
        DependencyStep::ExtractQuery {
            name,
            type_qualname,
            required,
            default_json,
        } => {
            let value = extract_query_step(
                ctx,
                name,
                type_qualname.as_str(),
                *required,
                default_json.as_ref(),
            )?;
            resolved.insert(name.clone(), value);
        }
        DependencyStep::ExtractHeader {
            name,
            alias,
            type_qualname,
            required,
        } => {
            let value = extract_header_step(ctx, name, alias, type_qualname.as_str(), *required)?;
            resolved.insert(name.clone(), value);
        }
        DependencyStep::ExtractCookie {
            name,
            type_qualname,
            required,
        } => {
            let value = extract_cookie_step(ctx, name, type_qualname.as_str(), *required)?;
            resolved.insert(name.clone(), value);
        }
        DependencyStep::ValidateBody {
            name,
            model_qualname,
        } => {
            let value = validate_body_step(ctx, model_qualname.as_str())?;
            resolved.insert(name.clone(), value);
        }
        DependencyStep::ResolveLifecycle {
            dep_qualname,
            target_kwarg,
        } => {
            let value = resolve_lifecycle_step(cache, dep_qualname.as_str())?;
            resolved.insert(target_kwarg.clone(), value);
        }
        DependencyStep::CallPython {
            dep_qualname,
            target_kwarg,
            inputs,
            is_async,
            ..
        } => {
            let value =
                call_python_step(dep_qualname.as_str(), inputs, *is_async, resolved).await?;
            resolved.insert(target_kwarg.clone(), value);
        }
        DependencyStep::ResolveNative { .. } => {
            return Err(AppError::Internal(
                "native dependency resolution not yet supported".to_owned(),
            ));
        }
    }
    Ok(())
}

// ── Step implementations ────────────────────────────────────────────────

/// Extract a path parameter from the request context.
fn extract_path_step(
    ctx: &RequestContext,
    name: &str,
    type_name: &str,
) -> Result<Py<PyAny>, AppError> {
    let raw = ctx
        .path_params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str());

    Python::attach(|py| match raw {
        Some(v) => convert_path_value(py, v, type_name).map(|b| b.unbind()),
        None => Err(AppError::BadRequest(format!(
            "missing path parameter: {name}"
        ))),
    })
}

/// Extract a query parameter from the request context.
fn extract_query_step(
    ctx: &RequestContext,
    name: &str,
    type_name: &str,
    required: bool,
    default_json: Option<&serde_json::Value>,
) -> Result<Py<PyAny>, AppError> {
    let raw = ctx
        .query_params
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str());

    Python::attach(|py| match raw {
        Some(v) => convert_path_value(py, v, type_name).map(|b| b.unbind()),
        None if !required => resolve_default(py, default_json),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["query".to_owned(), name.to_owned()],
            msg: "Field required".to_owned(),
            r#type: "missing".to_owned(),
        }])),
    })
}

/// Resolve a default value from JSON, falling back to `None`.
fn resolve_default(
    py: Python<'_>,
    default_json: Option<&serde_json::Value>,
) -> Result<Py<PyAny>, AppError> {
    match default_json {
        Some(serde_json::Value::String(s)) => Ok(PyString::new(py, s).into_any().unbind()),
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)
                    .map_err(|e| AppError::Internal(format!("int conversion: {e}")))?
                    .into_any()
                    .unbind())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)
                    .map_err(|e| AppError::Internal(format!("float conversion: {e}")))?
                    .into_any()
                    .unbind())
            } else {
                Ok(py.None())
            }
        }
        Some(serde_json::Value::Bool(b)) => {
            let py_bool = b
                .into_pyobject(py)
                .map_err(|e| AppError::Internal(format!("bool conversion: {e}")))?;
            Ok(py_bool.to_owned().into_any().unbind())
        }
        _ => Ok(py.None()),
    }
}

/// Extract a header value from the request context.
fn extract_header_step(
    ctx: &RequestContext,
    _name: &str,
    alias: &str,
    type_name: &str,
    required: bool,
) -> Result<Py<PyAny>, AppError> {
    let value = ctx.headers.get(alias).and_then(|v| v.to_str().ok());

    Python::attach(|py| match value {
        Some(v) => convert_path_value(py, v, type_name).map(|b| b.unbind()),
        None if !required => Ok(py.None()),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["header".to_owned(), alias.to_owned()],
            msg: format!("missing required header: {alias}"),
            r#type: "missing".to_owned(),
        }])),
    })
}

/// Extract a cookie value from the request context.
fn extract_cookie_step(
    ctx: &RequestContext,
    name: &str,
    type_name: &str,
    required: bool,
) -> Result<Py<PyAny>, AppError> {
    let cookie_header = ctx
        .headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let value = cookie_header.split(';').find_map(|pair| {
        let pair = pair.trim();
        pair.split_once('=')
            .filter(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim())
    });

    Python::attach(|py| match value {
        Some(v) => convert_path_value(py, v, type_name).map(|b| b.unbind()),
        None if !required => Ok(py.None()),
        None => Err(AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["cookie".to_owned(), name.to_owned()],
            msg: format!("missing required cookie: {name}"),
            r#type: "missing".to_owned(),
        }])),
    })
}

/// Validate request body via Pydantic `model_validate_json`.
fn validate_body_step(ctx: &RequestContext, model_qualname: &str) -> Result<Py<PyAny>, AppError> {
    // `name` from the step is only used as the resolved-map key; the caller
    // inserts the result under that key, so we don't need it here.
    let body = ctx
        .body
        .as_ref()
        .ok_or_else(|| AppError::Internal("body not read for ValidateBody step".to_owned()))?;

    Python::attach(|py| {
        let model_cls = import_qualified_name(py, model_qualname)
            .map_err(|e| AppError::Internal(format!("import model '{model_qualname}': {e}")))?;

        model_cls
            .bind(py)
            .call_method1(c"model_validate_json", (body.as_ref(),))
            .map(|result| result.unbind())
            .map_err(|e| {
                let errors = extract_pydantic_errors(py, &e);
                if errors.is_empty() {
                    AppError::BadRequest(format!("body validation failed: {e}"))
                } else {
                    AppError::Validation(errors)
                }
            })
    })
}

/// Look up a lifecycle-scoped dependency from the cache.
fn resolve_lifecycle_step(cache: &LifecycleCache, qualname: &str) -> Result<Py<PyAny>, AppError> {
    let value = cache.get(qualname).ok_or_else(|| {
        AppError::Internal(format!(
            "lifecycle dependency '{qualname}' not found in cache"
        ))
    })?;
    Python::attach(|py| Ok(value.clone_ref(py)))
}

/// Call a Python dependency function with kwargs from resolved inputs.
async fn call_python_step(
    dep_qualname: &str,
    inputs: &[String],
    is_async: bool,
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<Py<PyAny>, AppError> {
    if is_async {
        call_python_async(dep_qualname, inputs, resolved).await
    } else {
        call_python_sync(dep_qualname, inputs, resolved)
    }
}

/// Call a sync Python dependency.
fn call_python_sync(
    dep_qualname: &str,
    inputs: &[String],
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<Py<PyAny>, AppError> {
    Python::attach(|py| {
        let func = import_qualified_name(py, dep_qualname)
            .map_err(|e| AppError::Internal(format!("import dep '{dep_qualname}': {e}")))?;

        let kwargs = build_step_kwargs(py, inputs, resolved)?;

        func.call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("dep '{dep_qualname}' failed: {e}")))
    })
}

/// Call an async Python dependency.
async fn call_python_async(
    dep_qualname: &str,
    inputs: &[String],
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<Py<PyAny>, AppError> {
    let future = Python::attach(|py| {
        let func = import_qualified_name(py, dep_qualname)
            .map_err(|e| AppError::Internal(format!("import dep '{dep_qualname}': {e}")))?;

        let kwargs = build_step_kwargs(py, inputs, resolved)?;

        let coro = func
            .call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("dep '{dep_qualname}' failed: {e}")))?;

        pyo3_async_runtimes::tokio::into_future(coro.into_bound(py))
            .map_err(|e| AppError::Internal(format!("into_future: {e}")))
    })?;

    future
        .await
        .map_err(|e| AppError::Internal(format!("async dep '{dep_qualname}' failed: {e}")))
}

/// Build a kwargs dict for a dependency step from previously resolved values.
fn build_step_kwargs<'py>(
    py: Python<'py>,
    inputs: &[String],
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<pyo3::Bound<'py, pyo3::types::PyDict>, AppError> {
    let kwargs = pyo3::types::PyDict::new(py);
    for input_name in inputs {
        let value = resolved.get(input_name).ok_or_else(|| {
            AppError::Internal(format!(
                "unresolved input '{input_name}' for dependency step"
            ))
        })?;
        kwargs
            .set_item(input_name, value.bind(py))
            .map_err(|e| AppError::Internal(format!("set kwarg: {e}")))?;
    }
    Ok(kwargs)
}

// ── Handler invocation ──────────────────────────────────────────────────

/// Call the handler with plan-produced kwargs, filtering to declared names.
async fn invoke_with_kwargs(
    route: &BoundRoute,
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<Py<PyAny>, AppError> {
    let plan = route
        .manifest
        .dependency_plan
        .as_ref()
        .ok_or_else(|| AppError::Internal("missing dependency plan".to_owned()))?;

    let future = Python::attach(|py| {
        let kwargs = filter_handler_kwargs(py, &plan.handler_kwargs, resolved)?;

        let coro = route
            .handler
            .call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("handler call failed: {e}")))?;

        pyo3_async_runtimes::tokio::into_future(coro.into_bound(py))
            .map_err(|e| AppError::Internal(format!("into_future: {e}")))
    })?;

    future
        .await
        .map_err(|e| AppError::Internal(format!("handler failed: {e}")))
}

/// Filter resolved values to only the kwargs declared in the plan.
fn filter_handler_kwargs<'py>(
    py: Python<'py>,
    handler_kwargs: &[String],
    resolved: &HashMap<String, Py<PyAny>>,
) -> Result<pyo3::Bound<'py, pyo3::types::PyDict>, AppError> {
    let kwargs = pyo3::types::PyDict::new(py);
    for name in handler_kwargs {
        let value = resolved.get(name).ok_or_else(|| {
            AppError::Internal(format!("handler kwarg '{name}' not resolved by plan"))
        })?;
        kwargs
            .set_item(name, value.bind(py))
            .map_err(|e| AppError::Internal(format!("set kwarg: {e}")))?;
    }
    Ok(kwargs)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use crate::bridge::context::RequestContext;
    use crate::route::{DependencyPlan, DependencyStep, QualName};
    use http::HeaderMap;

    fn empty_ctx() -> RequestContext {
        RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
        }
    }

    #[tokio::test]
    async fn execute_empty_plan() {
        let plan = DependencyPlan {
            steps: Vec::new(),
            handler_kwargs: Vec::new(),
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn execute_extract_path() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractPath {
                name: "item_id".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
            }],
            handler_kwargs: vec!["item_id".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = RequestContext {
            path_params: vec![("item_id".to_owned(), "42".to_owned())],
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        assert!(result.contains_key("item_id"));
        Python::attach(|py| {
            let val: i64 = result["item_id"].extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[tokio::test]
    async fn execute_extract_query_required() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractQuery {
                name: "page".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
                required: true,
                default_json: None,
            }],
            handler_kwargs: vec!["page".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: vec![("page".to_owned(), "3".to_owned())],
            headers: HeaderMap::new(),
            body: None,
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: i64 = result["page"].extract(py).unwrap();
            assert_eq!(val, 3);
        });
    }

    #[tokio::test]
    async fn execute_extract_query_optional_missing() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractQuery {
                name: "page".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
                required: false,
                default_json: None,
            }],
            handler_kwargs: vec!["page".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            assert!(result["page"].bind(py).is_none());
        });
    }

    #[tokio::test]
    async fn execute_extract_header() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractHeader {
                name: "token".to_owned(),
                alias: "x-token".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: true,
            }],
            handler_kwargs: vec!["token".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-token", "secret".parse().unwrap());
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers,
            body: None,
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: String = result["token"].extract(py).unwrap();
            assert_eq!(val, "secret");
        });
    }

    #[tokio::test]
    async fn execute_extract_cookie() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractCookie {
                name: "session".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: true,
            }],
            handler_kwargs: vec!["session".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "session=abc123; theme=dark".parse().unwrap());
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers,
            body: None,
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: String = result["session"].extract(py).unwrap();
            assert_eq!(val, "abc123");
        });
    }

    #[tokio::test]
    async fn execute_resolve_lifecycle() {
        Python::initialize();
        let mut cache = LifecycleCache::empty();
        Python::attach(|py| {
            // Use private field access in test to populate cache.
            cache.values.insert("my.db.Engine".to_owned(), py.None());
        });

        let plan = DependencyPlan {
            steps: vec![DependencyStep::ResolveLifecycle {
                dep_qualname: QualName::new("my.db.Engine").unwrap(),
                target_kwarg: "engine".to_owned(),
            }],
            handler_kwargs: vec!["engine".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        assert!(result.contains_key("engine"));
    }

    #[tokio::test]
    async fn execute_resolve_lifecycle_missing() {
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ResolveLifecycle {
                dep_qualname: QualName::new("missing.Dep").unwrap(),
                target_kwarg: "dep".to_owned(),
            }],
            handler_kwargs: vec!["dep".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn execute_filter_handler_kwargs() {
        Python::initialize();
        let mut resolved = HashMap::new();
        Python::attach(|py| {
            resolved.insert("a".to_owned(), py.None());
            resolved.insert("b".to_owned(), py.None());
            resolved.insert("c".to_owned(), py.None());

            let kwargs = filter_handler_kwargs(py, &["a".to_owned(), "c".to_owned()], &resolved);
            let dict = kwargs.unwrap();
            assert_eq!(dict.len(), 2);
            assert!(dict.contains("a").unwrap());
            assert!(dict.contains("c").unwrap());
            assert!(!dict.contains("b").unwrap());
        });
    }

    #[test]
    fn plan_executor_dispatch_debug() {
        let cache = Arc::new(LifecycleCache::empty());
        let d = PlanExecutorDispatch::new(cache);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("PlanExecutorDispatch"));
    }

    #[tokio::test]
    async fn execute_extract_path_missing() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractPath {
                name: "id".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
            }],
            handler_kwargs: vec!["id".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn execute_resolve_native_unsupported() {
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ResolveNative {
                dep_qualname: QualName::new("native.Dep").unwrap(),
                target_kwarg: "dep".to_owned(),
                config: serde_json::Value::Null,
            }],
            handler_kwargs: vec!["dep".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[test]
    fn resolve_default_none() {
        Python::initialize();
        Python::attach(|py| {
            let result = resolve_default(py, None).unwrap();
            assert!(result.bind(py).is_none());
        });
    }

    #[test]
    fn resolve_default_string() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::Value::String("hello".to_owned());
            let result = resolve_default(py, Some(&default)).unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "hello");
        });
    }

    #[test]
    fn resolve_default_int() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::json!(42);
            let result = resolve_default(py, Some(&default)).unwrap();
            let val: i64 = result.extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn resolve_default_bool() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::Value::Bool(true);
            let result = resolve_default(py, Some(&default)).unwrap();
            let val: bool = result.extract(py).unwrap();
            assert!(val);
        });
    }

    #[test]
    fn resolve_default_float() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::json!(1.5);
            let result = resolve_default(py, Some(&default)).unwrap();
            let val: f64 = result.extract(py).unwrap();
            assert!((val - 1.5).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn resolve_default_null() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::Value::Null;
            let result = resolve_default(py, Some(&default)).unwrap();
            assert!(result.bind(py).is_none());
        });
    }

    #[test]
    fn resolve_default_array_falls_back_to_none() {
        Python::initialize();
        Python::attach(|py| {
            let default = serde_json::json!([1, 2, 3]);
            let result = resolve_default(py, Some(&default)).unwrap();
            assert!(result.bind(py).is_none());
        });
    }

    #[tokio::test]
    async fn execute_extract_query_required_missing() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractQuery {
                name: "page".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
                required: true,
                default_json: None,
            }],
            handler_kwargs: vec!["page".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_header_missing_required() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractHeader {
                name: "token".to_owned(),
                alias: "x-token".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: true,
            }],
            handler_kwargs: vec!["token".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_header_missing_optional() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractHeader {
                name: "token".to_owned(),
                alias: "x-token".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: false,
            }],
            handler_kwargs: vec!["token".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            assert!(result["token"].bind(py).is_none());
        });
    }

    #[tokio::test]
    async fn execute_extract_cookie_missing_required() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractCookie {
                name: "session".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: true,
            }],
            handler_kwargs: vec!["session".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_cookie_missing_optional() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractCookie {
                name: "session".to_owned(),
                type_qualname: QualName::new("str").unwrap(),
                required: false,
            }],
            handler_kwargs: vec!["session".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            assert!(result["session"].bind(py).is_none());
        });
    }

    #[test]
    fn filter_handler_kwargs_missing_key() {
        Python::initialize();
        Python::attach(|py| {
            let resolved = HashMap::new();
            let result = filter_handler_kwargs(py, &["missing".to_owned()], &resolved);
            assert!(result.is_err());
        });
    }

    #[tokio::test]
    async fn execute_extract_query_with_default() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractQuery {
                name: "page".to_owned(),
                type_qualname: QualName::new("int").unwrap(),
                required: false,
                default_json: Some(serde_json::json!(1)),
            }],
            handler_kwargs: vec!["page".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: i64 = result["page"].extract(py).unwrap();
            assert_eq!(val, 1);
        });
    }

    #[tokio::test]
    async fn execute_validate_body_step() {
        Python::initialize();
        // Create a minimal Pydantic model in Python for testing
        let has_pydantic = Python::attach(|py| py.import(c"pydantic").is_ok());
        if !has_pydantic {
            // Skip test if pydantic is not installed
            return;
        }

        Python::attach(|py| {
            py.run(
                c"import pydantic\nclass TestModel(pydantic.BaseModel):\n    name: str\n",
                None,
                None,
            )
            .unwrap();
        });

        let plan = DependencyPlan {
            steps: vec![DependencyStep::ValidateBody {
                name: "body".to_owned(),
                model_qualname: QualName::new("pydantic.BaseModel").unwrap(),
            }],
            handler_kwargs: vec!["body".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: Some(bytes::Bytes::from("{}")),
        };
        let cache = LifecycleCache::empty();
        // BaseModel accepts empty dict
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn execute_validate_body_missing_body() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ValidateBody {
                name: "body".to_owned(),
                model_qualname: QualName::new("pydantic.BaseModel").unwrap(),
            }],
            handler_kwargs: vec!["body".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx(); // no body
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[tokio::test]
    async fn execute_call_python_sync() {
        Python::initialize();
        // Use a built-in Python function: len
        let plan = DependencyPlan {
            steps: vec![DependencyStep::CallPython {
                dep_qualname: QualName::new("builtins.len").unwrap(),
                target_kwarg: "result".to_owned(),
                inputs: Vec::new(),
                is_generator: false,
                is_async: false,
                use_cache: false,
            }],
            handler_kwargs: vec!["result".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        // builtins.len() with no args will fail, but that exercises the call path
        let result = execute_plan(&plan, &ctx, &cache).await;
        // len() with no positional args and no kwargs fails — that's expected
        assert!(result.is_err());
    }

    #[test]
    fn build_step_kwargs_missing_input() {
        Python::initialize();
        Python::attach(|py| {
            let resolved = HashMap::new();
            let result = build_step_kwargs(py, &["missing".to_owned()], &resolved);
            assert!(result.is_err());
        });
    }

    #[test]
    fn build_step_kwargs_populated() {
        Python::initialize();
        Python::attach(|py| {
            let mut resolved = HashMap::new();
            resolved.insert("x".to_owned(), py.None());
            let kwargs = build_step_kwargs(py, &["x".to_owned()], &resolved).unwrap();
            assert_eq!(kwargs.len(), 1);
        });
    }

    #[tokio::test]
    async fn execute_validate_body_bad_import() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ValidateBody {
                name: "body".to_owned(),
                model_qualname: QualName::new("nonexistent.module.Model").unwrap(),
            }],
            handler_kwargs: vec!["body".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: Some(bytes::Bytes::from("{}")),
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[tokio::test]
    async fn execute_validate_body_invalid_json() {
        Python::initialize();
        let has_pydantic = Python::attach(|py| py.import(c"pydantic").is_ok());
        if !has_pydantic {
            return;
        }

        // Define a model that requires a string field
        Python::attach(|py| {
            py.run(
                c"import sys\nimport pydantic\nclass _TestModelVal(pydantic.BaseModel):\n    name: str\nsys.modules['_test_model_val'] = type(sys)('_test_model_val')\nsys.modules['_test_model_val']._TestModelVal = _TestModelVal\n",
                None,
                None,
            )
            .unwrap();
        });

        let plan = DependencyPlan {
            steps: vec![DependencyStep::ValidateBody {
                name: "body".to_owned(),
                model_qualname: QualName::new("_test_model_val._TestModelVal").unwrap(),
            }],
            handler_kwargs: vec!["body".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        // Invalid JSON — missing required "name" field
        let ctx = RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: Some(bytes::Bytes::from("{}")),
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        // Pydantic returns structured validation errors
        assert!(matches!(
            result.unwrap_err(),
            AppError::Validation(_) | AppError::BadRequest(_)
        ));
    }

    #[tokio::test]
    async fn execute_call_python_sync_success() {
        Python::initialize();
        // Define a simple sync function that returns a value
        Python::attach(|py| {
            py.run(
                c"import sys\ndef _test_sync_fn(x=None): return 42\nsys.modules['_test_sync_mod'] = type(sys)('_test_sync_mod')\nsys.modules['_test_sync_mod']._test_sync_fn = _test_sync_fn\n",
                None,
                None,
            )
            .unwrap();
        });

        let plan = DependencyPlan {
            steps: vec![DependencyStep::CallPython {
                dep_qualname: QualName::new("_test_sync_mod._test_sync_fn").unwrap(),
                target_kwarg: "result".to_owned(),
                inputs: Vec::new(),
                is_generator: false,
                is_async: false,
                use_cache: false,
            }],
            handler_kwargs: vec!["result".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: i64 = result["result"].extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[tokio::test]
    async fn execute_call_python_sync_with_inputs() {
        Python::initialize();
        // Define a sync function that uses its inputs
        Python::attach(|py| {
            py.run(
                c"import sys\ndef _test_add(a=0, b=0): return a + b\nsys.modules['_test_add_mod'] = type(sys)('_test_add_mod')\nsys.modules['_test_add_mod']._test_add = _test_add\n",
                None,
                None,
            )
            .unwrap();
        });

        // Pre-resolve "a" and "b" via ExtractPath, then call the function
        let plan = DependencyPlan {
            steps: vec![
                DependencyStep::ExtractPath {
                    name: "a".to_owned(),
                    type_qualname: QualName::new("int").unwrap(),
                },
                DependencyStep::ExtractPath {
                    name: "b".to_owned(),
                    type_qualname: QualName::new("int").unwrap(),
                },
                DependencyStep::CallPython {
                    dep_qualname: QualName::new("_test_add_mod._test_add").unwrap(),
                    target_kwarg: "sum".to_owned(),
                    inputs: vec!["a".to_owned(), "b".to_owned()],
                    is_generator: false,
                    is_async: false,
                    use_cache: false,
                },
            ],
            handler_kwargs: vec!["sum".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = RequestContext {
            path_params: vec![
                ("a".to_owned(), "3".to_owned()),
                ("b".to_owned(), "7".to_owned()),
            ],
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
        };
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await.unwrap();
        Python::attach(|py| {
            let val: i64 = result["sum"].extract(py).unwrap();
            assert_eq!(val, 10);
        });
    }

    #[tokio::test]
    async fn execute_call_python_sync_bad_import() {
        Python::initialize();
        let plan = DependencyPlan {
            steps: vec![DependencyStep::CallPython {
                dep_qualname: QualName::new("nonexistent.module.func").unwrap(),
                target_kwarg: "result".to_owned(),
                inputs: Vec::new(),
                is_generator: false,
                is_async: false,
                use_cache: false,
            }],
            handler_kwargs: vec!["result".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let ctx = empty_ctx();
        let cache = LifecycleCache::empty();
        let result = execute_plan(&plan, &ctx, &cache).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }
}
