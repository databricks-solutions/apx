//! Cloneable handle for submitting coroutines to the persistent event loop.
//!
//! [`EventLoopHandle`] is the main interface used by dispatch code. It's
//! cheaply cloneable (`Arc`-backed) and safe to use from any Tokio task.

use super::scheduling::{CoroutineScheduler, TaskCallback};
use crate::error::AppError;
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::oneshot;

/// Cloneable handle to the persistent asyncio event loop.
///
/// Used by dispatch code to submit Python coroutines. Each call to
/// [`drive_coroutine`](Self::drive_coroutine) schedules the coroutine on
/// the event loop thread via `call_soon_threadsafe` and returns a Tokio
/// future that resolves when the coroutine completes.
///
pub struct EventLoopHandle {
    event_loop: Py<PyAny>,
    running: Arc<AtomicBool>,
}

impl Clone for EventLoopHandle {
    fn clone(&self) -> Self {
        Python::attach(|py| Self {
            event_loop: self.event_loop.clone_ref(py),
            running: Arc::clone(&self.running),
        })
    }
}

impl EventLoopHandle {
    /// Create a new handle. Called by [`super::core::EventLoop::handle`].
    pub fn new(event_loop: Py<PyAny>, running: Arc<AtomicBool>) -> Self {
        Self {
            event_loop,
            running,
        }
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
            let _span = tracing::trace_span!("drive_coroutine_schedule").entered();
            let callback = Py::new(py, TaskCallback::new(tx))
                .map_err(|e| AppError::Internal(format!("TaskCallback: {e}")))?;
            let scheduler = Py::new(py, CoroutineScheduler::new(coro, callback.into_any()))
                .map_err(|e| AppError::Internal(format!("CoroutineScheduler: {e}")))?;

            self.event_loop
                .call_method1(py, "call_soon_threadsafe", (scheduler,))
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
            let scheduler = Py::new(py, CoroutineScheduler::new(coro, callback.into_any()))
                .map_err(|e| AppError::Internal(format!("CoroutineScheduler: {e}")))?;

            self.event_loop
                .call_method1(py, "call_soon_threadsafe", (scheduler,))
                .map_err(|e| AppError::Internal(format!("call_soon_threadsafe: {e}")))?;
            Ok(())
        })?;

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
        let handle = event_loop.handle();

        // Create a trivial coroutine: `async def _t(): return 42`
        let coro = Python::attach(|py| {
            let code =
                std::ffi::CString::new("async def _t():\n    return 42\ncoro = _t()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
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
        let handle = event_loop.handle();

        // Coroutine that uses asyncio.sleep (requires running event loop).
        // Import inside the function body so the reference is captured in the closure.
        let coro = Python::attach(|py| {
            let code = std::ffi::CString::new(
                "async def _t():\n    import asyncio\n    await asyncio.sleep(0)\n    return 'ok'\ncoro = _t()\n",
            )
            .unwrap();
            let locals = pyo3::types::PyDict::new(py);
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
        let handle = event_loop.handle();

        let coro = Python::attach(|py| {
            let code = std::ffi::CString::new(
                "async def _t():\n    raise ValueError('test error')\ncoro = _t()\n",
            )
            .unwrap();
            let locals = pyo3::types::PyDict::new(py);
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
        let handle = event_loop.handle();
        event_loop.stop();

        let coro = Python::attach(|py| {
            let code =
                std::ffi::CString::new("async def _t():\n    return 1\ncoro = _t()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
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
        let handle = event_loop.handle();
        let dbg = format!("{handle:?}");
        assert!(dbg.contains("EventLoopHandle"));
        event_loop.stop();
    }
}
