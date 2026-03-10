//! Cloneable handle for submitting coroutines to the persistent event loop.
//!
//! [`EventLoopHandle`] is the main interface used by dispatch code. It's
//! cheaply cloneable (`Arc`-backed) and safe to use from any Tokio task.

use super::scheduling::{CoroutineScheduler, TaskCallback};
use crate::error::AppError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::oneshot;

/// Cloneable handle to the persistent asyncio event loop.
///
/// Caches bound Python methods (`call_soon_threadsafe`, `create_task`)
/// resolved once at startup. Request-path scheduling uses direct calls
/// to the cached callables — no per-request attribute lookup.
pub struct EventLoopHandle {
    event_loop: Py<PyAny>,
    /// Cached `loop.call_soon_threadsafe` bound method.
    call_soon: Py<PyAny>,
    /// Cached `loop.create_task` bound method.
    create_task: Py<PyAny>,
    running: Arc<AtomicBool>,
}

impl Clone for EventLoopHandle {
    fn clone(&self) -> Self {
        Python::attach(|py| Self {
            event_loop: self.event_loop.clone_ref(py),
            call_soon: self.call_soon.clone_ref(py),
            create_task: self.create_task.clone_ref(py),
            running: Arc::clone(&self.running),
        })
    }
}

impl EventLoopHandle {
    /// Create a new handle with cached bound methods.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop is missing expected methods.
    pub fn new(event_loop: Py<PyAny>, running: Arc<AtomicBool>) -> Result<Self, String> {
        Python::attach(|py| {
            let loop_obj = event_loop.bind(py);
            let call_soon = loop_obj
                .getattr(c"call_soon_threadsafe")
                .map_err(|e| format!("event loop missing call_soon_threadsafe: {e}"))?
                .unbind();
            let create_task = loop_obj
                .getattr(c"create_task")
                .map_err(|e| format!("event loop missing create_task: {e}"))?
                .unbind();
            Ok(Self {
                event_loop,
                call_soon,
                create_task,
                running,
            })
        })
    }

    /// Submit a Python coroutine to the running event loop.
    ///
    /// Returns a Tokio future that resolves when the coroutine completes.
    /// The coroutine runs on the event loop thread with full asyncio context
    /// (BackgroundTasks, contextvars, get_running_loop).
    ///
    /// # Errors
    ///
    /// - `AppError::Internal` if the event loop is stopped or scheduling fails.
    pub async fn drive_coroutine(&self, coro: Py<PyAny>) -> Result<Py<PyAny>, AppError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(AppError::Internal("event loop is not running".to_owned()));
        }

        let (tx, rx) = oneshot::channel();

        // Single brief GIL acquisition — consistent with asgi_dispatch.rs:58.
        // call_soon_threadsafe is O(1) and thread-safe by design; the GIL hold
        // covers only 2 object allocations + enqueue (<5µs).
        Python::attach(|py| -> Result<(), AppError> {
            let callback = Py::new(py, TaskCallback::new(tx))
                .map_err(|e| AppError::Internal(format!("TaskCallback: {e}")))?;
            let ct = self.create_task.clone_ref(py);
            let scheduler = Py::new(py, CoroutineScheduler::new(coro, callback.into_any(), ct))
                .map_err(|e| AppError::Internal(format!("CoroutineScheduler: {e}")))?;

            self.call_soon
                .call1(py, (scheduler,))
                .map_err(|e| AppError::Internal(format!("call_soon_threadsafe: {e}")))?;
            Ok(())
        })?;

        let t0 = std::time::Instant::now();
        let result = rx.await.map_err(|_| {
            AppError::Internal("event loop closed before coroutine completed".to_owned())
        })?;
        tracing::trace!(
            elapsed_us = t0.elapsed().as_micros(),
            "drive_coroutine_await"
        );
        result
    }

    /// Build a coroutine and schedule it in a single GIL hold.
    ///
    /// Combines scope construction and event loop scheduling into one
    /// `Python::attach` call, eliminating a GIL acquire/release cycle
    /// compared to separate `build` + `drive_coroutine` calls.
    ///
    /// Returns a receiver that resolves when the coroutine completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop is stopped, the closure fails,
    /// or scheduling fails.
    pub fn schedule_with<F>(
        &self,
        f: F,
    ) -> Result<oneshot::Receiver<Result<Py<PyAny>, AppError>>, AppError>
    where
        F: FnOnce(Python<'_>) -> Result<Py<PyAny>, AppError>,
    {
        if !self.running.load(Ordering::Acquire) {
            return Err(AppError::Internal("event loop is not running".to_owned()));
        }

        let (tx, rx) = oneshot::channel();

        Python::attach(|py| -> Result<(), AppError> {
            let coro = f(py)?;

            let callback = Py::new(py, TaskCallback::new(tx))
                .map_err(|e| AppError::Internal(format!("TaskCallback: {e}")))?;
            let ct = self.create_task.clone_ref(py);
            let scheduler = Py::new(py, CoroutineScheduler::new(coro, callback.into_any(), ct))
                .map_err(|e| AppError::Internal(format!("CoroutineScheduler: {e}")))?;

            self.call_soon
                .call1(py, (scheduler,))
                .map_err(|e| AppError::Internal(format!("call_soon_threadsafe: {e}")))?;
            Ok(())
        })?;

        Ok(rx)
    }

    /// Defer all Python work to the event loop thread.
    ///
    /// Unlike [`schedule_with`](Self::schedule_with) which builds the coroutine
    /// on the calling (tokio) thread with GIL held, this method only acquires
    /// the GIL briefly to enqueue a lightweight closure via `call_soon_threadsafe`.
    /// The closure runs entirely on the event loop thread, reducing GIL
    /// contention from concurrent tokio tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if the event loop is stopped or enqueue fails.
    pub fn schedule_deferred<F>(
        &self,
        f: F,
    ) -> Result<oneshot::Receiver<Result<Py<PyAny>, AppError>>, AppError>
    where
        F: FnOnce(Python<'_>) -> Result<Py<PyAny>, AppError> + Send + 'static,
    {
        if !self.running.load(Ordering::Acquire) {
            return Err(AppError::Internal("event loop is not running".to_owned()));
        }

        let trace = crate::bridge::bench_trace_enabled();

        let (tx, rx) = oneshot::channel();

        // Wrap the builder + oneshot sender in Mutex<Option<...>> for FnOnce-in-Fn.
        let work = std::sync::Mutex::new(Some((f, tx)));

        // Capture enqueue timestamp for cross-thread pickup delay measurement.
        let enqueued_at = trace.then(std::time::Instant::now);

        // Single brief GIL hold: build PyCFunction closure + enqueue via
        // call_soon_threadsafe. The closure runs on the event loop thread.
        let t_gil = trace.then(std::time::Instant::now);
        Python::attach(|py| -> Result<(), AppError> {
            let create_task = self.create_task.clone_ref(py);
            let deferred = PyCFunction::new_closure(
                py,
                None,
                None,
                move |args: &Bound<'_, PyTuple>,
                      _kwargs: Option<&Bound<'_, PyDict>>|
                      -> PyResult<()> {
                    // Measure cross-thread pickup delay (enqueue → closure execution).
                    if let Some(enqueued_at) = enqueued_at {
                        tracing::info!(
                            target: "bench_trace",
                            phase = "cross_thread_pickup",
                            pickup_delay_us = enqueued_at.elapsed().as_micros(),
                        );
                    }

                    let py = args.py();
                    let (f, tx) = work
                        .lock()
                        .map_err(|_| {
                            pyo3::exceptions::PyRuntimeError::new_err("deferred dispatch poisoned")
                        })?
                        .take()
                        .ok_or_else(|| {
                            pyo3::exceptions::PyRuntimeError::new_err(
                                "deferred dispatch already consumed",
                            )
                        })?;

                    let coro = f(py)
                        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;

                    let callback = Py::new(py, TaskCallback::new(tx))?;
                    let task = create_task.call1(py, (coro,))?;
                    task.call_method1(py, c"add_done_callback", (callback,))?;
                    Ok(())
                },
            )
            .map_err(|e| AppError::Internal(format!("deferred closure: {e}")))?;

            self.call_soon
                .call1(py, (deferred,))
                .map_err(|e| AppError::Internal(format!("call_soon_threadsafe: {e}")))?;
            Ok(())
        })?;

        if let Some(t_gil) = t_gil {
            tracing::info!(
                target: "bench_trace",
                phase = "schedule_deferred_gil",
                gil_hold_us = t_gil.elapsed().as_micros(),
            );
        }

        Ok(rx)
    }

    /// Get a reference to the event loop Python object (for tests/diagnostics).
    pub fn event_loop(&self) -> &Py<PyAny> {
        &self.event_loop
    }
}

impl std::fmt::Debug for EventLoopHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoopHandle")
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::event_loop::core::EventLoop;

    #[tokio::test]
    async fn drive_trivial_coroutine() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        let handle = event_loop.handle().unwrap();

        // Create a trivial coroutine: `async def _t(): return 42`
        let coro = Python::attach(|py| {
            let code =
                std::ffi::CString::new("async def _t():\n    return 42\ncoro = _t()\n").unwrap();
            let locals = PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            locals.get_item("coro").unwrap().unwrap().unbind()
        });

        let result = handle.drive_coroutine(coro).await.unwrap();
        Python::attach(|py| {
            let val: i64 = result.extract(py).unwrap();
            assert_eq!(val, 42);
        });

        event_loop.stop();
    }

    #[tokio::test]
    async fn drive_coroutine_with_await() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        let handle = event_loop.handle().unwrap();

        // Coroutine that uses asyncio.sleep (requires running event loop).
        // Import inside the function body so the reference is captured in the closure.
        let coro = Python::attach(|py| {
            let code = std::ffi::CString::new(
                "async def _t():\n    import asyncio\n    await asyncio.sleep(0)\n    return 'ok'\ncoro = _t()\n",
            )
            .unwrap();
            let locals = PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            locals.get_item("coro").unwrap().unwrap().unbind()
        });

        let result = handle.drive_coroutine(coro).await.unwrap();
        Python::attach(|py| {
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "ok");
        });

        event_loop.stop();
    }

    #[tokio::test]
    async fn drive_coroutine_exception() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        let handle = event_loop.handle().unwrap();

        let coro = Python::attach(|py| {
            let code = std::ffi::CString::new(
                "async def _t():\n    raise ValueError('test error')\ncoro = _t()\n",
            )
            .unwrap();
            let locals = PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            locals.get_item("coro").unwrap().unwrap().unbind()
        });

        let result = handle.drive_coroutine(coro).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("internal error") || err_msg.contains("test error"));

        event_loop.stop();
    }

    #[tokio::test]
    async fn drive_after_stop_fails() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        let handle = event_loop.handle().unwrap();
        event_loop.stop();

        let coro = Python::attach(|py| {
            let code =
                std::ffi::CString::new("async def _t():\n    return 1\ncoro = _t()\n").unwrap();
            let locals = PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            locals.get_item("coro").unwrap().unwrap().unbind()
        });

        let result = handle.drive_coroutine(coro).await;
        assert!(result.is_err());
    }

    #[test]
    fn handle_debug() {
        crate::with_py(|_py| {});
        let mut event_loop = EventLoop::start().unwrap();
        let handle = event_loop.handle().unwrap();
        let dbg = format!("{handle:?}");
        assert!(dbg.contains("EventLoopHandle"));
        event_loop.stop();
    }
}
