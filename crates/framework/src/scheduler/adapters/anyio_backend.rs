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
//! `anyio.abc.AsyncBackend` and delegates to the Rust core. Complex features
//! (cancel scopes, task groups, memory object streams) fall back to the stock
//! `asyncio` backend via `__getattr__`.

use std::sync::Arc;

use pyo3::prelude::*;

use super::super::driver::CachedTypes;
use super::super::primitives::{BlockingTask, RustEvent, RustFuture, Timer};

// ---------------------------------------------------------------------------
// ApxSchedulerCore -- the Rust pyclass backing the AnyIO adapter
// ---------------------------------------------------------------------------

/// Rust-backed scheduler core exposed to Python.
///
/// Implements the hot-path methods that the embedded `ApxBackend` Python class
/// delegates to. Methods that are too complex to implement natively (cancel
/// scopes, task groups) return `None` to signal that the Python layer should
/// fall back to the stock asyncio backend.
#[pyclass(module = "apx._core")]
pub struct ApxSchedulerCore {
    #[expect(
        dead_code,
        reason = "passed to driver during full anyio dispatch wiring"
    )]
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
    fn sleep(&self, delay: f64) -> Timer {
        Timer::new(delay)
    }

    /// Create a new async event flag.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn create_event(&self) -> RustEvent {
        RustEvent::new()
    }

    /// Return an awaitable that immediately resolves with `None`.
    ///
    /// This is the anyio checkpoint -- it yields once to let other tasks run.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn checkpoint(&self, py: Python<'_>) -> RustFuture {
        RustFuture::resolved(py.None())
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
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Spawn actual blocking work on a thread that acquires the GIL.
        std::thread::spawn(move || {
            let result = Python::attach(|py| func.call0(py));
            // Best-effort send -- if the receiver is dropped, the result is discarded.
            let _ = tx.send(result);
        });

        Ok(BlockingTask::with_receiver(rx))
    }

    /// Return a reference to `self` as the current scheduler token.
    fn current_token(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Return `asyncio.CancelledError` for use as the cancelled exception class.
    #[allow(
        clippy::unused_self,
        reason = "Python instance method — &self required by protocol"
    )]
    fn cancelled_exception_class(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let cls = py.import(c"asyncio")?.getattr(c"CancelledError")?.unbind();
        Ok(cls)
    }
}

// ---------------------------------------------------------------------------
// Embedded Python glue
// ---------------------------------------------------------------------------

/// Python source for the `ApxBackend` class that inherits from
/// `anyio.abc.AsyncBackend` and delegates to the Rust `ApxSchedulerCore`.
///
/// Complex anyio features (cancel scopes, task groups, memory object streams)
/// fall back to the stock `asyncio` backend. The `__getattr__` method catches
/// anything we don't explicitly implement.
const BACKEND_GLUE: &str = r#"
from anyio.abc import AsyncBackend

class ApxBackend(AsyncBackend):
    def __init__(self, core):
        self._core = core
        self._fallback = None

    async def sleep(self, delay):
        return await self._core.sleep(delay)

    def create_event(self):
        return self._core.create_event()

    def create_cancel_scope(self, *, deadline=float('inf'), shield=False):
        return self._get_fallback().create_cancel_scope(deadline=deadline, shield=shield)

    def create_task_group(self):
        return self._get_fallback().create_task_group()

    async def run_sync_in_worker_thread(self, func, *, abandon_on_cancel=False, limiter=None):
        return await self._core.run_sync_in_worker_thread(func, abandon_on_cancel)

    def create_memory_object_stream(self, max_buffer_size=0, item_type=None):
        return self._get_fallback().create_memory_object_stream(max_buffer_size, item_type=item_type)

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
/// The returned object inherits from `anyio.abc.AsyncBackend` and can be used
/// wherever anyio expects a backend instance. Registration with anyio's plugin
/// system is handled by the integration layer (not here).
#[expect(
    dead_code,
    reason = "called when anyio backend registration is wired up"
)]
pub fn create_backend(py: Python<'_>, core: &Py<ApxSchedulerCore>) -> PyResult<Py<PyAny>> {
    let code = std::ffi::CString::new(BACKEND_GLUE)?;
    let locals = pyo3::types::PyDict::new(py);
    locals.set_item("core", core)?;
    py.run(&code, None, Some(&locals))?;

    let backend_cls = locals.get_item("ApxBackend")?.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("ApxBackend class not found after eval")
    })?;

    // Instantiate: ApxBackend(core)
    let instance = backend_cls.call1((core,))?;
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
            let timer = core.sleep(1.0);
            assert!(!timer.done());
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
            let future = core.checkpoint(py);
            assert!(future.done());
        });
    }

    #[test]
    fn cancelled_exception_class_is_asyncio_cancelled_error() {
        crate::with_py(|py| {
            let core = ApxSchedulerCore::new(py).unwrap();
            let cls = core.cancelled_exception_class(py).unwrap();
            let asyncio_cls = py
                .import(c"asyncio")
                .unwrap()
                .getattr(c"CancelledError")
                .unwrap();
            assert!(cls.bind(py).is(&asyncio_cls));
        });
    }
}
