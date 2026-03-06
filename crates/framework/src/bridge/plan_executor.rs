//! Compiled dependency plan executor dispatch.
//!
//! Executes pre-compiled [`DependencyPlan`] steps to resolve handler kwargs,
//! then calls the handler directly. No FastAPI dependency solving at runtime.

use super::context::RequestContext;
use super::dispatch::{
    AppState, HandlerDispatch, extract_context, extract_pydantic_errors, serialize_result,
};
use crate::discovery::bind::import_qualified_name;
use crate::error::AppError;
use crate::event_loop::EventLoopHandle;
use crate::route::{BoundDependencyPlan, BoundRoute, DependencyStep};
use crate::runtime::lifecycle::LifecycleCache;
use crate::transport::types::{InboundRequest, OutboundResponse};
use pyo3::types::{PyAnyMethods, PyDict, PyDictMethods};
use pyo3::{Py, PyAny, Python};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Resolved dependency values, keyed by target kwarg name.
///
/// Populated during plan execution, consumed to build the handler's kwargs dict.
#[derive(Debug)]
struct ResolvedKwargs(HashMap<String, Py<PyAny>>);

impl ResolvedKwargs {
    fn with_capacity(n: usize) -> Self {
        Self(HashMap::with_capacity(n))
    }

    /// Insert a resolved value.
    ///
    /// Debug-asserts that the key hasn't been written before (plan steps are
    /// topologically sorted — double-writes indicate a plan compilation bug).
    fn insert(&mut self, key: String, value: Py<PyAny>) {
        debug_assert!(
            !self.0.contains_key(&key),
            "plan executor: duplicate resolved key '{key}'"
        );
        self.0.insert(key, value);
    }

    fn get(&self, key: &str) -> Option<&Py<PyAny>> {
        self.0.get(key)
    }

    /// Build a PyDict containing only the specified kwargs.
    fn to_py_dict<'py>(
        &self,
        py: Python<'py>,
        names: &[String],
    ) -> Result<pyo3::Bound<'py, PyDict>, AppError> {
        let dict = PyDict::new(py);
        for name in names {
            let value = self.get(name).ok_or_else(|| {
                AppError::Internal(format!("kwarg '{name}' not resolved by plan"))
            })?;
            dict.set_item(name, value.bind(py))
                .map_err(|e| AppError::Internal(format!("set kwarg '{name}': {e}")))?;
        }
        Ok(dict)
    }

    /// Check if empty.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Check if a key exists.
    #[cfg(test)]
    fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
}

impl std::ops::Index<&str> for ResolvedKwargs {
    type Output = Py<PyAny>;

    fn index(&self, key: &str) -> &Self::Output {
        &self.0[key]
    }
}

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

            let bound_plan = route.bound_plan.as_ref().ok_or_else(|| {
                AppError::Internal("route has no bound dependency plan".to_owned())
            })?;

            let kwargs = execute_plan(bound_plan, &ctx, &cache, &app_state.loop_handle).await?;
            let result =
                invoke_with_kwargs(&route, bound_plan, &kwargs, &app_state.loop_handle).await?;
            Python::attach(|py| serialize_result(py, &result, &route))
        })
    }
}

/// Execute all steps in a dependency plan, producing resolved values.
async fn execute_plan(
    bound_plan: &BoundDependencyPlan,
    ctx: &RequestContext,
    cache: &LifecycleCache,
    loop_handle: &EventLoopHandle,
) -> Result<ResolvedKwargs, AppError> {
    let mut resolved = ResolvedKwargs::with_capacity(bound_plan.plan.steps.len());

    for (index, step) in bound_plan.plan.steps.iter().enumerate() {
        let callable = bound_plan.callable_for(index);
        let (key, value) = resolve_step(step, callable, ctx, cache, &resolved, loop_handle).await?;
        resolved.insert(key.to_owned(), value);
    }

    Ok(resolved)
}

/// Resolve a single dependency step, returning the target kwarg name and value.
async fn resolve_step<'a>(
    step: &'a DependencyStep,
    callable: Option<&Py<PyAny>>,
    ctx: &RequestContext,
    cache: &LifecycleCache,
    resolved: &ResolvedKwargs,
    loop_handle: &EventLoopHandle,
) -> Result<(&'a str, Py<PyAny>), AppError> {
    match step {
        DependencyStep::ExtractPath {
            name,
            type_qualname,
        } => {
            let value = extract_path_step(ctx, name, type_qualname.as_str())?;
            Ok((name.as_str(), value))
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
            Ok((name.as_str(), value))
        }
        DependencyStep::ExtractHeader {
            name,
            alias,
            type_qualname,
            required,
        } => {
            let value = extract_header_step(ctx, name, alias, type_qualname.as_str(), *required)?;
            Ok((name.as_str(), value))
        }
        DependencyStep::ExtractCookie {
            name,
            type_qualname,
            required,
        } => {
            let value = extract_cookie_step(ctx, name, type_qualname.as_str(), *required)?;
            Ok((name.as_str(), value))
        }
        DependencyStep::ValidateBody {
            name,
            model_qualname,
        } => {
            let value = validate_body_step(ctx, model_qualname.as_str())?;
            Ok((name.as_str(), value))
        }
        DependencyStep::ResolveLifecycle {
            dep_qualname,
            target_kwarg,
        } => {
            let value = resolve_lifecycle_step(cache, dep_qualname.as_str())?;
            Ok((target_kwarg.as_str(), value))
        }
        DependencyStep::CallPython {
            target_kwarg,
            inputs,
            is_async,
            ..
        } => {
            let func = callable.ok_or_else(|| {
                AppError::Internal("missing pre-resolved callable for CallPython step".to_owned())
            })?;
            let value = call_python_step(func, inputs, *is_async, resolved, loop_handle).await?;
            Ok((target_kwarg.as_str(), value))
        }
        DependencyStep::ResolveNative { .. } => Err(AppError::Internal(
            "native dependency resolution not yet supported".to_owned(),
        )),
    }
}

// ── Step implementations ────────────────────────────────────────────────

/// Extract a path parameter from the request context.
fn extract_path_step(
    ctx: &RequestContext,
    name: &str,
    type_name: &str,
) -> Result<Py<PyAny>, AppError> {
    Python::attach(|py| {
        super::extract::extract_path_value(py, &ctx.path_params, name, type_name, true)
            .map(|b| b.unbind())
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
    Python::attach(|py| {
        super::extract::extract_query_value(
            py,
            &ctx.query_params,
            name,
            type_name,
            required,
            default_json,
        )
        .map(|b| b.unbind())
    })
}

/// Extract a header value from the request context.
fn extract_header_step(
    ctx: &RequestContext,
    _name: &str,
    alias: &str,
    type_name: &str,
    required: bool,
) -> Result<Py<PyAny>, AppError> {
    Python::attach(|py| {
        super::extract::extract_header_value(py, &ctx.headers, alias, type_name, required)
            .map(|b| b.unbind())
    })
}

/// Extract a cookie value from the request context.
fn extract_cookie_step(
    ctx: &RequestContext,
    name: &str,
    type_name: &str,
    required: bool,
) -> Result<Py<PyAny>, AppError> {
    Python::attach(|py| {
        super::extract::extract_cookie_value(py, &ctx.headers, name, type_name, required)
            .map(|b| b.unbind())
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
    func: &Py<PyAny>,
    inputs: &[String],
    is_async: bool,
    resolved: &ResolvedKwargs,
    loop_handle: &EventLoopHandle,
) -> Result<Py<PyAny>, AppError> {
    if is_async {
        call_python_async(func, inputs, resolved, loop_handle).await
    } else {
        call_python_sync(func, inputs, resolved)
    }
}

/// Call a sync Python dependency.
fn call_python_sync(
    func: &Py<PyAny>,
    inputs: &[String],
    resolved: &ResolvedKwargs,
) -> Result<Py<PyAny>, AppError> {
    Python::attach(|py| {
        let kwargs = resolved.to_py_dict(py, inputs)?;
        func.call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("dependency call failed: {e}")))
    })
}

/// Call an async Python dependency via the persistent event loop.
async fn call_python_async(
    func: &Py<PyAny>,
    inputs: &[String],
    resolved: &ResolvedKwargs,
    loop_handle: &EventLoopHandle,
) -> Result<Py<PyAny>, AppError> {
    let coro = Python::attach(|py| {
        let kwargs = resolved.to_py_dict(py, inputs)?;
        func.call(py, (), Some(&kwargs))
            .map_err(|e| AppError::Internal(format!("dependency call failed: {e}")))
    })?;

    loop_handle
        .drive_coroutine(coro)
        .await
        .map_err(|e| AppError::Internal(format!("async dependency failed: {e}")))
}

// ── Handler invocation ──────────────────────────────────────────────────

/// Call the handler with plan-produced kwargs, filtering to declared names.
async fn invoke_with_kwargs(
    route: &BoundRoute,
    bound_plan: &BoundDependencyPlan,
    resolved: &ResolvedKwargs,
    loop_handle: &EventLoopHandle,
) -> Result<Py<PyAny>, AppError> {
    if route.manifest.is_async_handler {
        let coro = Python::attach(|py| {
            let kwargs = resolved.to_py_dict(py, &bound_plan.plan.handler_kwargs)?;
            route
                .handler
                .call(py, &kwargs)
                .map(|b| b.unbind())
                .map_err(|e| AppError::Internal(format!("handler call failed: {e}")))
        })?;
        loop_handle
            .drive_coroutine(coro)
            .await
            .map_err(|e| AppError::Internal(format!("handler failed: {e}")))
    } else {
        let (handler, kwargs) = Python::attach(|py| {
            let kwargs = resolved.to_py_dict(py, &bound_plan.plan.handler_kwargs)?;
            Ok::<_, AppError>((route.handler.clone_ref(py), kwargs.unbind()))
        })?;
        tokio::task::spawn_blocking(move || {
            Python::attach(|py| {
                handler
                    .call(py, kwargs.bind(py))
                    .map(|b| b.unbind())
                    .map_err(|e| AppError::Internal(format!("handler failed: {e}")))
            })
        })
        .await
        .map_err(|e| AppError::Internal(format!("spawn_blocking: {e}")))?
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use crate::bridge::context::RequestContext;
    use crate::route::{DependencyPlan, DependencyStep, QualName};
    use crate::with_py;
    use http::HeaderMap;

    fn test_handle() -> EventLoopHandle {
        // Start a real event loop. The EventLoop is leaked (stopped on process exit)
        // because these tests exercise sync-only plan steps. The handle keeps it alive.
        let event_loop = crate::event_loop::EventLoop::start().unwrap();
        let handle = event_loop.handle();
        std::mem::forget(event_loop);
        handle
    }

    fn empty_ctx() -> RequestContext {
        RequestContext {
            path_params: Vec::new(),
            query_params: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
        }
    }

    /// Wrap a `DependencyPlan` into a `BoundDependencyPlan`.
    ///
    /// Resolves `CallPython` step qualnames to live Python callables (mirrors
    /// `bind_dependency_plan` in `discovery/bind.rs`). Non-`CallPython` steps
    /// get `None`.
    fn bind_plan(plan: DependencyPlan) -> BoundDependencyPlan {
        let callables = with_py(|py| {
            plan.steps
                .iter()
                .map(|step| match step {
                    DependencyStep::CallPython { dep_qualname, .. } => {
                        import_qualified_name(py, dep_qualname.as_str()).ok()
                    }
                    _ => None,
                })
                .collect()
        });
        BoundDependencyPlan { plan, callables }
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn execute_extract_path() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            assert!(result.contains_key("item_id"));
            let val: i64 = result["item_id"].extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[tokio::test]
    async fn execute_extract_query_required() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: i64 = result["page"].extract(py).unwrap();
            assert_eq!(val, 3);
        });
    }

    #[tokio::test]
    async fn execute_extract_query_optional_missing() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            assert!(result["page"].bind(py).is_none());
        });
    }

    #[tokio::test]
    async fn execute_extract_header() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: String = result["token"].extract(py).unwrap();
            assert_eq!(val, "secret");
        });
    }

    #[tokio::test]
    async fn execute_extract_cookie() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: String = result["session"].extract(py).unwrap();
            assert_eq!(val, "abc123");
        });
    }

    #[tokio::test]
    async fn execute_resolve_lifecycle() {
        let mut cache = LifecycleCache::empty();
        with_py(|py| {
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn resolved_kwargs_to_py_dict() {
        with_py(|py| {
            let mut resolved = ResolvedKwargs::with_capacity(3);
            resolved.insert("a".to_owned(), py.None());
            resolved.insert("b".to_owned(), py.None());
            resolved.insert("c".to_owned(), py.None());

            let dict = resolved
                .to_py_dict(py, &["a".to_owned(), "c".to_owned()])
                .unwrap();
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
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[test]
    fn resolve_default_none() {
        with_py(|py| {
            let result = crate::bridge::extract::resolve_default(py, None).unwrap();
            assert!(result.bind(py).is_none());
        });
    }

    #[test]
    fn resolve_default_string() {
        with_py(|py| {
            let default = serde_json::Value::String("hello".to_owned());
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "hello");
        });
    }

    #[test]
    fn resolve_default_int() {
        with_py(|py| {
            let default = serde_json::json!(42);
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let val: i64 = result.extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn resolve_default_bool() {
        with_py(|py| {
            let default = serde_json::Value::Bool(true);
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let val: bool = result.extract(py).unwrap();
            assert!(val);
        });
    }

    #[test]
    fn resolve_default_float() {
        with_py(|py| {
            let default = serde_json::json!(1.5);
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let val: f64 = result.extract(py).unwrap();
            assert!((val - 1.5).abs() < f64::EPSILON);
        });
    }

    #[test]
    fn resolve_default_null() {
        with_py(|py| {
            let default = serde_json::Value::Null;
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            assert!(result.bind(py).is_none());
        });
    }

    #[test]
    fn resolve_default_array_roundtrips() {
        with_py(|py| {
            let default = serde_json::json!([1, 2, 3]);
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let val: Vec<i64> = result.extract(py).unwrap();
            assert_eq!(val, vec![1, 2, 3]);
        });
    }

    #[test]
    fn resolve_default_object_roundtrips() {
        with_py(|py| {
            let default = serde_json::json!({"key": "val"});
            let result = crate::bridge::extract::resolve_default(py, Some(&default)).unwrap();
            let dict = result.bind(py).cast::<PyDict>().unwrap();
            let val: String = dict.get_item("key").unwrap().unwrap().extract().unwrap();
            assert_eq!(val, "val");
        });
    }

    #[tokio::test]
    async fn execute_extract_query_required_missing() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_header_missing_required() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_header_missing_optional() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            assert!(result["token"].bind(py).is_none());
        });
    }

    #[tokio::test]
    async fn execute_extract_cookie_missing_required() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Validation(_)));
    }

    #[tokio::test]
    async fn execute_extract_cookie_missing_optional() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            assert!(result["session"].bind(py).is_none());
        });
    }

    #[test]
    fn resolved_kwargs_to_py_dict_missing_key() {
        with_py(|py| {
            let resolved = ResolvedKwargs::with_capacity(0);
            let result = resolved.to_py_dict(py, &["missing".to_owned()]);
            assert!(result.is_err());
        });
    }

    #[tokio::test]
    async fn execute_extract_query_with_default() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: i64 = result["page"].extract(py).unwrap();
            assert_eq!(val, 1);
        });
    }

    #[tokio::test]
    async fn execute_validate_body_step() {
        // Create a minimal Pydantic model in Python for testing
        let has_pydantic = with_py(|py| py.import(c"pydantic").is_ok());
        if !has_pydantic {
            // Skip test if pydantic is not installed
            return;
        }

        with_py(|py| {
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn execute_validate_body_missing_body() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[tokio::test]
    async fn execute_call_python_sync() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        // len() with no positional args and no kwargs fails — that's expected
        assert!(result.is_err());
    }

    #[test]
    fn resolved_kwargs_missing_input() {
        with_py(|py| {
            let resolved = ResolvedKwargs::with_capacity(0);
            let result = resolved.to_py_dict(py, &["missing".to_owned()]);
            assert!(result.is_err());
        });
    }

    #[test]
    fn resolved_kwargs_populated() {
        with_py(|py| {
            let mut resolved = ResolvedKwargs::with_capacity(1);
            resolved.insert("x".to_owned(), py.None());
            let kwargs = resolved.to_py_dict(py, &["x".to_owned()]).unwrap();
            assert_eq!(kwargs.len(), 1);
        });
    }

    #[tokio::test]
    async fn execute_validate_body_bad_import() {
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }

    #[tokio::test]
    async fn execute_validate_body_invalid_json() {
        let has_pydantic = with_py(|py| py.import(c"pydantic").is_ok());
        if !has_pydantic {
            return;
        }

        // Define a model that requires a string field
        with_py(|py| {
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        // Pydantic returns structured validation errors
        assert!(matches!(
            result.unwrap_err(),
            AppError::Validation(_) | AppError::BadRequest(_)
        ));
    }

    #[tokio::test]
    async fn execute_call_python_sync_success() {
        // Define a simple sync function that returns a value
        with_py(|py| {
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: i64 = result["result"].extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[tokio::test]
    async fn execute_call_python_sync_with_inputs() {
        // Define a sync function that uses its inputs
        with_py(|py| {
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle())
            .await
            .unwrap();
        with_py(|py| {
            let val: i64 = result["sum"].extract(py).unwrap();
            assert_eq!(val, 10);
        });
    }

    #[tokio::test]
    async fn execute_call_python_unresolved_callable() {
        // With pre-resolution, a bad qualname produces None at bind time.
        // The executor surfaces this as an Internal error.
        with_py(|_py| {});
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
        let bound = bind_plan(plan);
        let result = execute_plan(&bound, &ctx, &cache, &test_handle()).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Internal(_)));
    }
}
