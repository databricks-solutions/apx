//! Per-worker lifecycle dependency cache.
//!
//! Stores values resolved once per worker lifetime (e.g. DB engine, config).
//! Initialization calls each lifecycle dependency callable, handles sync/async
//! generators (yield-based deps), and stores cleanup references for shutdown.

use crate::discovery::bind::import_qualified_name;
use crate::route::LifecycleDepManifest;
use pyo3::types::PyAnyMethods;
use pyo3::{Py, PyAny, Python};
use std::collections::HashMap;

/// Errors during lifecycle dependency initialization.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// Failed to import a lifecycle dependency callable.
    #[error("import lifecycle dep '{qualname}': {message}")]
    Import {
        /// Python qualname that could not be imported.
        qualname: String,
        /// Underlying error detail.
        message: String,
    },
    /// Failed to initialize a lifecycle dependency.
    #[error("initialize lifecycle dep '{qualname}': {message}")]
    Init {
        /// Python qualname of the dependency that failed.
        qualname: String,
        /// Underlying error detail.
        message: String,
    },
}

/// Kind of generator stored for shutdown cleanup.
#[derive(Debug, Clone, Copy)]
enum GenKind {
    Sync,
    Async,
}

/// Python callable classification for lifecycle dependency init.
#[derive(Debug)]
enum CallableKind {
    AsyncGenerator,
    SyncGenerator,
    AsyncFunction,
    SyncFunction,
}

/// Result of initializing one lifecycle dep: resolved value + optional generator for cleanup.
type InitResult = Result<(Py<PyAny>, Option<(Py<PyAny>, GenKind)>), LifecycleError>;

/// Cache of lifecycle-scoped dependency values, shared across all routes.
///
/// Populated during worker startup, read during request dispatch by
/// [`PlanExecutorDispatch`](crate::bridge::plan_executor::PlanExecutorDispatch).
#[derive(Debug)]
pub struct LifecycleCache {
    /// Resolved values keyed by qualname. `pub(crate)` for test access.
    pub(crate) values: HashMap<String, Py<PyAny>>,
    /// Generators needing cleanup at shutdown, in initialization order.
    exit_stacks: Vec<(String, Py<PyAny>, GenKind)>,
}

impl LifecycleCache {
    /// Create an empty cache (no lifecycle deps resolved yet).
    pub fn empty() -> Self {
        Self {
            values: HashMap::new(),
            exit_stacks: Vec::new(),
        }
    }

    /// Initialize lifecycle dependencies by importing and calling each callable.
    ///
    /// Sorts deps by `init_order`, detects callable type (sync/async,
    /// plain/generator), and stores yielded values and generator handles.
    ///
    /// # Errors
    ///
    /// Returns an error if any dependency cannot be imported or initialized.
    pub fn initialize(
        py: Python<'_>,
        deps: &[LifecycleDepManifest],
    ) -> Result<Self, LifecycleError> {
        if deps.is_empty() {
            return Ok(Self::empty());
        }

        let mut sorted: Vec<_> = deps.iter().collect();
        sorted.sort_by_key(|d| d.init_order);

        let inspect = py.import(c"inspect").map_err(|e| LifecycleError::Import {
            qualname: "inspect".to_owned(),
            message: format!("import inspect module: {e}"),
        })?;

        let mut values = HashMap::with_capacity(sorted.len());
        let mut exit_stacks = Vec::new();

        for dep in &sorted {
            let qualname = dep.qualname.as_str();
            let (value, cleanup) = init_dep(py, &inspect, qualname)?;
            values.insert(qualname.to_owned(), value);
            if let Some((generator, kind)) = cleanup {
                exit_stacks.push((qualname.to_owned(), generator, kind));
            }
        }

        Ok(Self {
            values,
            exit_stacks,
        })
    }

    /// Look up a cached lifecycle dependency by qualified name.
    pub fn get(&self, qualname: &str) -> Option<&Py<PyAny>> {
        self.values.get(qualname)
    }

    /// Shut down lifecycle dependencies in reverse initialization order.
    ///
    /// Uses reverse init order (not `shutdown_order`) because exit stacks
    /// are naturally ordered by initialization — reversing gives correct
    /// teardown sequencing without an extra sort.
    ///
    /// Advances each generator past its yield point to trigger cleanup.
    /// Errors are logged but not propagated (best-effort cleanup).
    pub fn shutdown(&self, py: Python<'_>) {
        for (qualname, generator, kind) in self.exit_stacks.iter().rev() {
            cleanup_generator(py, qualname, generator, *kind);
        }
    }
}

// ── Initialization helpers ──────────────────────────────────────────────

/// Initialize a single lifecycle dependency, detecting its callable type.
fn init_dep(py: Python<'_>, inspect: &pyo3::Bound<'_, PyAny>, qualname: &str) -> InitResult {
    let func = import_qualified_name(py, qualname).map_err(|e| LifecycleError::Import {
        qualname: qualname.to_owned(),
        message: e.to_string(),
    })?;

    let kind = detect_callable_kind(inspect, func.bind(py), qualname)?;

    match kind {
        CallableKind::AsyncGenerator => init_async_gen(py, qualname, &func),
        CallableKind::SyncGenerator => init_sync_gen(py, qualname, &func),
        CallableKind::AsyncFunction => init_async_fn(py, qualname, &func),
        CallableKind::SyncFunction => init_sync_fn(py, qualname, &func),
    }
}

/// Classify a Python callable using `inspect` predicates.
fn detect_callable_kind(
    inspect: &pyo3::Bound<'_, PyAny>,
    func: &pyo3::Bound<'_, PyAny>,
    qualname: &str,
) -> Result<CallableKind, LifecycleError> {
    if inspect_predicate(inspect, c"isasyncgenfunction", func, qualname)? {
        return Ok(CallableKind::AsyncGenerator);
    }
    if inspect_predicate(inspect, c"isgeneratorfunction", func, qualname)? {
        return Ok(CallableKind::SyncGenerator);
    }
    if inspect_predicate(inspect, c"iscoroutinefunction", func, qualname)? {
        return Ok(CallableKind::AsyncFunction);
    }
    Ok(CallableKind::SyncFunction)
}

/// Call an `inspect` predicate (e.g. `isasyncgenfunction`) on a callable.
fn inspect_predicate(
    inspect: &pyo3::Bound<'_, PyAny>,
    method: &std::ffi::CStr,
    func: &pyo3::Bound<'_, PyAny>,
    qualname: &str,
) -> Result<bool, LifecycleError> {
    inspect
        .call_method1(method, (func,))
        .and_then(|r| r.is_truthy())
        .map_err(|e| LifecycleError::Init {
            qualname: qualname.to_owned(),
            message: format!("inspect check: {e}"),
        })
}

/// Initialize an async generator dep: call → `await __anext__()` → store gen.
fn init_async_gen(py: Python<'_>, qualname: &str, func: &Py<PyAny>) -> InitResult {
    let generator = call_lifecycle_fn(py, qualname, func)?;
    let anext_coro =
        generator
            .call_method0(py, c"__anext__")
            .map_err(|e| LifecycleError::Init {
                qualname: qualname.to_owned(),
                message: format!("__anext__: {e}"),
            })?;
    let value = run_coroutine(py, &anext_coro).map_err(|e| LifecycleError::Init {
        qualname: qualname.to_owned(),
        message: format!("await __anext__: {e}"),
    })?;
    Ok((value, Some((generator, GenKind::Async))))
}

/// Initialize a sync generator dep: call → `__next__()` → store gen.
fn init_sync_gen(py: Python<'_>, qualname: &str, func: &Py<PyAny>) -> InitResult {
    let generator = call_lifecycle_fn(py, qualname, func)?;
    let value = generator
        .call_method0(py, c"__next__")
        .map_err(|e| LifecycleError::Init {
            qualname: qualname.to_owned(),
            message: format!("__next__: {e}"),
        })?;
    Ok((value, Some((generator, GenKind::Sync))))
}

/// Initialize an async callable dep: call → await coroutine.
fn init_async_fn(py: Python<'_>, qualname: &str, func: &Py<PyAny>) -> InitResult {
    let coro = call_lifecycle_fn(py, qualname, func)?;
    let value = run_coroutine(py, &coro).map_err(|e| LifecycleError::Init {
        qualname: qualname.to_owned(),
        message: format!("await: {e}"),
    })?;
    Ok((value, None))
}

/// Initialize a plain sync callable dep: call → store result.
fn init_sync_fn(py: Python<'_>, qualname: &str, func: &Py<PyAny>) -> InitResult {
    let value = call_lifecycle_fn(py, qualname, func)?;
    Ok((value, None))
}

/// Call a lifecycle callable with no arguments.
fn call_lifecycle_fn(
    py: Python<'_>,
    qualname: &str,
    func: &Py<PyAny>,
) -> Result<Py<PyAny>, LifecycleError> {
    func.call0(py).map_err(|e| LifecycleError::Init {
        qualname: qualname.to_owned(),
        message: format!("call: {e}"),
    })
}

/// Run a Python coroutine on the current asyncio event loop.
fn run_coroutine(py: Python<'_>, coro: &Py<PyAny>) -> Result<Py<PyAny>, pyo3::PyErr> {
    let asyncio = py.import(c"asyncio")?;
    let loop_obj = asyncio.call_method0(c"get_event_loop")?;
    let result = loop_obj.call_method1(c"run_until_complete", (coro.bind(py),))?;
    Ok(result.unbind())
}

// ── Shutdown helpers ────────────────────────────────────────────────────

/// Advance a generator past its yield to trigger cleanup code.
fn cleanup_generator(py: Python<'_>, qualname: &str, generator: &Py<PyAny>, kind: GenKind) {
    let result = match kind {
        GenKind::Sync => cleanup_sync_gen(py, generator),
        GenKind::Async => cleanup_async_gen(py, generator),
    };
    if let Err(e) = result {
        tracing::warn!(dep = qualname, error = %e, "lifecycle dep cleanup failed");
    }
}

/// Advance a sync generator; `StopIteration` is the expected outcome.
fn cleanup_sync_gen(py: Python<'_>, generator: &Py<PyAny>) -> Result<(), pyo3::PyErr> {
    match generator.call_method0(py, c"__next__") {
        Ok(_) => Ok(()),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Advance an async generator; `StopAsyncIteration` is the expected outcome.
fn cleanup_async_gen(py: Python<'_>, generator: &Py<PyAny>) -> Result<(), pyo3::PyErr> {
    let anext_coro = generator.call_method0(py, c"__anext__")?;
    match run_coroutine(py, &anext_coro) {
        Ok(_) => Ok(()),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py) => Ok(()),
        Err(e) => Err(e),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use crate::route::{DepScope, QualName};
    use crate::with_py;

    #[test]
    fn lifecycle_error_display_import() {
        let err = LifecycleError::Import {
            qualname: "my.module.func".to_owned(),
            message: "module not found".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("import lifecycle dep"));
        assert!(msg.contains("my.module.func"));
    }

    #[test]
    fn lifecycle_error_display_init() {
        let err = LifecycleError::Init {
            qualname: "my.module.func".to_owned(),
            message: "call failed".to_owned(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("initialize lifecycle dep"));
        assert!(msg.contains("my.module.func"));
    }

    #[test]
    fn lifecycle_cache_empty_returns_none() {
        let cache = LifecycleCache::empty();
        assert!(cache.get("some.dep").is_none());
    }

    #[test]
    fn lifecycle_cache_get_stored_value() {
        with_py(|py| {
            let mut cache = LifecycleCache::empty();
            cache.values.insert("my.dep".to_owned(), py.None());
            assert!(cache.get("my.dep").is_some());
        });
    }

    #[test]
    fn lifecycle_cache_get_missing_key() {
        with_py(|py| {
            let mut cache = LifecycleCache::empty();
            cache.values.insert("other.dep".to_owned(), py.None());
            assert!(cache.get("missing.dep").is_none());
        });
    }

    #[test]
    fn lifecycle_cache_initialize_empty_deps() {
        with_py(|py| {
            let cache = LifecycleCache::initialize(py, &[]).unwrap();
            assert!(cache.values.is_empty());
            assert!(cache.exit_stacks.is_empty());
        });
    }

    #[test]
    fn lifecycle_cache_initialize_sync_callable() {
        with_py(|py| {
            let dep = LifecycleDepManifest {
                qualname: QualName::new("os.getpid").unwrap(),
                init_order: 0,
                shutdown_order: 0,
                scope: DepScope::Worker,
            };
            let cache = LifecycleCache::initialize(py, &[dep]).unwrap();
            assert!(cache.get("os.getpid").is_some());
            assert!(cache.exit_stacks.is_empty());
        });
    }

    #[test]
    fn lifecycle_cache_shutdown_empty() {
        with_py(|py| {
            let cache = LifecycleCache::empty();
            cache.shutdown(py);
        });
    }

    #[test]
    fn lifecycle_cache_shutdown_reverse_order() {
        with_py(|py| {
            py.run(
                c"
import types, sys
m = types.ModuleType('_lc_test_order')
m.order = []
def gen_a():
    yield 'a'
    m.order.append('a')
def gen_b():
    yield 'b'
    m.order.append('b')
m.gen_a = gen_a
m.gen_b = gen_b
sys.modules['_lc_test_order'] = m
",
                None,
                None,
            )
            .unwrap();

            let deps = [
                LifecycleDepManifest {
                    qualname: QualName::new("_lc_test_order.gen_a").unwrap(),
                    init_order: 0,
                    shutdown_order: 1,
                    scope: DepScope::Worker,
                },
                LifecycleDepManifest {
                    qualname: QualName::new("_lc_test_order.gen_b").unwrap(),
                    init_order: 1,
                    shutdown_order: 0,
                    scope: DepScope::Worker,
                },
            ];

            let cache = LifecycleCache::initialize(py, &deps).unwrap();
            assert!(cache.get("_lc_test_order.gen_a").is_some());
            assert!(cache.get("_lc_test_order.gen_b").is_some());
            assert_eq!(cache.exit_stacks.len(), 2);

            cache.shutdown(py);

            let module = py.import(c"_lc_test_order").unwrap();
            let order = module.getattr(c"order").unwrap();
            let order_list: Vec<String> = order.extract().unwrap();
            assert_eq!(order_list, vec!["b", "a"]);
        });
    }

    #[test]
    fn lifecycle_cache_get_after_init() {
        with_py(|py| {
            let dep = LifecycleDepManifest {
                qualname: QualName::new("os.getcwd").unwrap(),
                init_order: 0,
                shutdown_order: 0,
                scope: DepScope::Worker,
            };
            let cache = LifecycleCache::initialize(py, &[dep]).unwrap();
            assert!(cache.get("os.getcwd").is_some());
        });
    }
}
