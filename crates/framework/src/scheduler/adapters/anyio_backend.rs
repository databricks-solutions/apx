//! AnyIO backend adapter -- delegates to the Rust scheduler core.
//!
//! Starlette/FastAPI never call asyncio directly; they go through anyio.
//! By providing a custom anyio backend, we intercept those calls and route
//! them through our Rust primitives.
//!
//! # Architecture
//!
//! A [`ApxSchedulerCore`] pyclass holds the method implementations. The
//! Python `ApxBackend` class (in `src/apx/_backend/`) inherits from
//! `anyio.abc.AsyncBackend` and delegates to the Rust core. CancelScope,
//! TaskGroup, MemoryObjectStream, sync primitives, task introspection,
//! thread bridge, process/signal, and entry points are implemented natively.
//! Networking methods delegate explicitly to the stock asyncio backend.

use std::sync::Arc;

use pyo3::prelude::*;
use tokio::sync::oneshot;

use super::super::driver::CachedTypes;
use super::super::primitives::{BlockingTask, Event, Future, Lock, Semaphore, Timer};
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

    /// Create a new Rust-backed async Lock.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_lock_primitive(&self) -> Lock {
        Lock::new()
    }

    /// Create a new Rust-backed counting Semaphore.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_semaphore_primitive(&self, permits: u32) -> Semaphore {
        Semaphore::new(permits)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create an `ApxBackend` instance wrapping the given scheduler core.
///
/// Imports `apx._backend` and calls `create_backend(core)` to construct
/// the Python backend object. The returned object inherits from
/// `anyio.abc.AsyncBackend` and can be used wherever anyio expects a backend.
#[expect(
    dead_code,
    reason = "called when anyio backend registration is wired up"
)]
pub fn create_backend(py: Python<'_>, core: &Py<ApxSchedulerCore>) -> PyResult<Py<PyAny>> {
    let factory = py.import(c"apx._backend")?.getattr(c"create_backend")?;
    let backend = factory.call1((core,))?;
    Ok(backend.unbind())
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
