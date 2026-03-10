//! AnyIO backend adapter -- delegates to the Rust scheduler core.
//!
//! Starlette/FastAPI never call asyncio directly; they go through anyio.
//! By providing a custom anyio backend, we intercept those calls and route
//! them through our Rust primitives.
//!
//! # Architecture
//!
//! A [`ApxSchedulerCore`] pyclass holds the method implementations. A small
//! Python class (embedded as [`BACKEND_GLUE`]) inherits from
//! `anyio.abc.AsyncBackend` and delegates to the Rust core. CancelScope,
//! TaskGroup, and MemoryObjectStream are implemented natively. Remaining
//! features fall back to the stock `asyncio` backend via `__getattr__`.

use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::oneshot;

use super::super::driver::CachedTypes;
use super::super::primitives::{BlockingTask, Event, Future, Timer};
use super::cancel_scope::CancelScopeState;
use super::task_group::TaskGroupCore;

// ---------------------------------------------------------------------------
// ApxSchedulerCore -- the Rust pyclass backing the AnyIO adapter
// ---------------------------------------------------------------------------

/// One-shot callable that resolves a [`Future`] when invoked.
///
/// Used by [`ApxSchedulerCore::checkpoint`] as the `call_soon` target
/// to ensure the future resolves on the next event loop iteration.
#[pyclass(module = "apx._core")]
struct CheckpointResolver {
    tx: Option<oneshot::Sender<Py<PyAny>>>,
}

#[pymethods]
impl CheckpointResolver {
    fn __call__(&mut self) {
        if let Some(tx) = self.tx.take() {
            Python::attach(|py| {
                let _ = tx.send(py.None());
            });
        }
    }
}

/// Rust-backed scheduler core exposed to Python.
///
/// Implements the hot-path methods that the embedded `ApxBackend` Python class
/// delegates to. CancelScope, TaskGroup, and MemoryObjectStream are now
/// implemented natively instead of falling back to asyncio.
#[pyclass(module = "apx._core")]
pub struct ApxSchedulerCore {
    cached_types: Arc<CachedTypes>,
    epoch: std::time::Instant,
}

impl std::fmt::Debug for ApxSchedulerCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApxSchedulerCore")
            .field("elapsed", &self.epoch.elapsed())
            .finish()
    }
}

#[pymethods]
impl ApxSchedulerCore {
    #[new]
    fn new(py: Python<'_>) -> PyResult<Self> {
        let cached_types = Arc::new(CachedTypes::resolve(py)?);
        Ok(Self {
            cached_types,
            epoch: std::time::Instant::now(),
        })
    }

    /// Return an awaitable timer that fires after `delay` seconds.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn sleep(&self, py: Python<'_>, delay: f64) -> PyResult<Timer> {
        Timer::new(py, delay)
    }

    /// Create a new async event flag.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_event(&self) -> Event {
        Event::new()
    }

    /// Return an awaitable that resolves on the next event loop tick.
    ///
    /// This is the anyio checkpoint — yields once to let other tasks run.
    /// The future is resolved via `loop.call_soon`, ensuring one event loop
    /// iteration passes before the coroutine resumes.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn checkpoint(&self, py: Python<'_>) -> PyResult<Py<Future>> {
        let asyncio = py.import(c"asyncio")?;
        match asyncio.call_method0(c"get_running_loop") {
            Ok(event_loop) => {
                let (future, tx) = Future::with_channel();
                let py_future = Py::new(py, future)?;
                let resolver = Py::new(py, CheckpointResolver { tx: Some(tx) })?;
                event_loop.call_method1(c"call_soon", (resolver,))?;
                Ok(py_future)
            }
            Err(_) => {
                // No running loop (test/diagnostic context) — resolve immediately.
                Ok(Py::new(py, Future::resolved(py.None()))?)
            }
        }
    }

    /// Return elapsed time since the scheduler was created.
    fn current_time(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// Spawn a callable on a blocking thread and return an awaitable.
    ///
    /// The callable is invoked with the GIL held on a separate thread.
    /// `abandon_on_cancel` is accepted for API compatibility but not yet
    /// honoured (the blocking work always runs to completion).
    #[pyo3(signature = (func, abandon_on_cancel=false))]
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn run_sync_in_worker_thread(
        &self,
        func: Py<PyAny>,
        abandon_on_cancel: bool,
    ) -> PyResult<BlockingTask> {
        let _ = abandon_on_cancel; // accepted for API compat, not yet honoured
        let (tx, rx) = oneshot::channel();

        // Prefer tokio's blocking pool (backpressure, reuse). Fall back to
        // raw thread if no tokio runtime is available (e.g. tests).
        let handle = crate::scheduler::with_tokio_handle(tokio::runtime::Handle::clone);
        let work = move || {
            let result = Python::attach(|py| func.call0(py));
            let _ = tx.send(result);
        };
        if let Some(handle) = handle {
            handle.spawn_blocking(work);
        } else {
            std::thread::spawn(work);
        }

        Ok(BlockingTask::with_receiver(rx))
    }

    /// Return a reference to `self` as the current scheduler token.
    fn current_token(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Return `asyncio.CancelledError` for use as the cancelled exception class.
    fn cancelled_exception_class(&self, py: Python<'_>) -> Py<PyAny> {
        self.cached_types
            .cancelled_error_cls
            .clone_ref(py)
            .into_any()
    }

    /// Create a new `CancelScopeState` sharing this core's epoch.
    #[pyo3(signature = (deadline=f64::INFINITY, shield=false))]
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_cancel_scope_state(&self, deadline: f64, shield: bool) -> CancelScopeState {
        CancelScopeState::new(deadline, shield)
    }

    /// Create a new `TaskGroupCore`.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_task_group_core(&self, py: Python<'_>) -> PyResult<TaskGroupCore> {
        TaskGroupCore::new(py)
    }
}

// ---------------------------------------------------------------------------
// Embedded Python glue
// ---------------------------------------------------------------------------

/// Python source for the `ApxBackend` class that inherits from
/// `anyio.abc.AsyncBackend` and delegates to the Rust `ApxSchedulerCore`.
///
/// CancelScope, TaskGroup, and MemoryObjectStream are implemented natively.
/// The `__getattr__` fallback handles remaining features (networking, files,
/// thread integration).
const BACKEND_GLUE: &str = r#"
from anyio.abc import AsyncBackend

class ApxBackend(AsyncBackend):
    def __init__(self, core, cancel_scope_cls, task_group_cls, create_stream_pair):
        self._core = core
        self._cancel_scope_cls = cancel_scope_cls
        self._task_group_cls = task_group_cls
        self._create_stream_pair = create_stream_pair
        self._fallback = None

    async def sleep(self, delay):
        return await self._core.sleep(delay)

    def create_event(self):
        return self._core.create_event()

    def create_cancel_scope(self, *, deadline=float('inf'), shield=False):
        state = self._core.create_cancel_scope_state(deadline, shield)
        return self._cancel_scope_cls(state)

    def create_task_group(self):
        core = self._core.create_task_group_core()
        # Create a cancel scope for the task group
        state = self._core.create_cancel_scope_state()
        scope = self._cancel_scope_cls(state)
        return self._task_group_cls(core, scope)

    async def run_sync_in_worker_thread(self, func, *, abandon_on_cancel=False, limiter=None):
        return await self._core.run_sync_in_worker_thread(func, abandon_on_cancel)

    def create_memory_object_stream(self, max_buffer_size=0, item_type=None):
        return self._create_stream_pair(max_buffer_size, self._core.create_event)

    async def checkpoint(self):
        return await self._core.checkpoint()

    def current_time(self):
        return self._core.current_time()

    def current_token(self):
        return self._core.current_token()

    @property
    def cancelled_exception_class(self):
        return self._core.cancelled_exception_class()

    def _get_fallback(self):
        if self._fallback is None:
            from anyio._backends._asyncio import AsyncIOBackend
            self._fallback = AsyncIOBackend()
        return self._fallback

    def __getattr__(self, name):
        return getattr(self._get_fallback(), name)
"#;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create an `ApxBackend` instance wrapping the given scheduler core.
///
/// Evaluates all Python glue code (cancel scope, task group, memory stream)
/// and wires them into the backend. The returned object inherits from
/// `anyio.abc.AsyncBackend` and can be used wherever anyio expects a backend.
#[expect(
    dead_code,
    reason = "called when anyio backend registration is wired up"
)]
pub fn create_backend(py: Python<'_>, core: &Py<ApxSchedulerCore>) -> PyResult<Py<PyAny>> {
    // Helper: extract a key from a glue dict (Py<PyAny> wrapping a PyDict).
    fn get_glue_item<'py>(
        dict: &Bound<'py, pyo3::types::PyDict>,
        key: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        dict.get_item(key)?.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("{key} not found in glue"))
        })
    }

    // Evaluate cancel scope glue
    let cs_dict = super::cancel_scope::eval_cancel_scope_glue(py)?
        .into_bound(py)
        .cast_into::<pyo3::types::PyDict>()?;
    let cancel_scope_cls = get_glue_item(&cs_dict, "ApxCancelScope")?;

    // Evaluate task group glue
    let tg_dict = super::task_group::eval_task_group_glue(py)?
        .into_bound(py)
        .cast_into::<pyo3::types::PyDict>()?;
    let task_group_cls = get_glue_item(&tg_dict, "ApxTaskGroup")?;

    // Evaluate memory stream glue
    let ms_dict = super::memory_stream::eval_memory_stream_glue(py)?
        .into_bound(py)
        .cast_into::<pyo3::types::PyDict>()?;
    let create_stream_pair = get_glue_item(&ms_dict, "create_memory_object_stream_pair")?;

    // Evaluate the backend glue
    let code = std::ffi::CString::new(BACKEND_GLUE)?;
    let locals = pyo3::types::PyDict::new(py);
    locals.set_item("core", core)?;
    locals.set_item("cancel_scope_cls", &cancel_scope_cls)?;
    locals.set_item("task_group_cls", &task_group_cls)?;
    locals.set_item("create_stream_pair", &create_stream_pair)?;
    py.run(&code, None, Some(&locals))?;

    let backend_cls = locals.get_item("ApxBackend")?.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("ApxBackend class not found after eval")
    })?;

    // Instantiate: ApxBackend(core, cancel_scope_cls, task_group_cls, create_stream_pair)
    let instance = backend_cls.call1((
        core,
        &cancel_scope_cls,
        &task_group_cls,
        &create_stream_pair,
    ))?;
    Ok(instance.unbind())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn core_construction() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            // current_time should be very small (just created).
            let t = core.current_time();
            assert!(t < 1.0, "expected sub-second elapsed, got {t}");
        });
    }

    #[test]
    fn sleep_returns_timer() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let timer = core.sleep(py, 1.0).unwrap();
            assert!(!timer.done(py));
        });
    }

    #[test]
    fn create_event_returns_event() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let event = core.create_event();
            assert!(!event.is_set());
        });
    }

    #[test]
    fn checkpoint_returns_resolved_future() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            // No running event loop in test → falls back to immediate resolution.
            let future = core.checkpoint(py).unwrap();
            assert!(future.borrow(py).done());
        });
    }

    #[test]
    fn cancelled_exception_class_is_asyncio_cancelled_error() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let cls = core.cancelled_exception_class(py);
            let asyncio_cls = py
                .import(c"asyncio")
                .unwrap()
                .getattr(c"CancelledError")
                .unwrap();
            assert!(cls.bind(py).is(&asyncio_cls));
        });
    }

    #[test]
    fn create_cancel_scope_state_returns_state() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let state = core.create_cancel_scope_state(10.0, true);
            assert!(!state.is_effectively_cancelled());
        });
    }

    #[test]
    fn create_task_group_core_returns_core() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let tg_core = core.create_task_group_core(py).unwrap();
            assert!(!tg_core.has_pending());
        });
    }
}
