//! Cloneable handle for submitting coroutines to the persistent event loop.
//!
//! [`EventLoopHandle`] is the main interface used by dispatch code. It's
//! cheaply cloneable and safe to use from any Tokio task.
//!
//! The hot path (`schedule_deferred`) pushes work items to a crossbeam
//! channel with zero GIL acquisition. Driver threads consume items and
//! drive coroutines concurrently.

use crate::driver_pool::{DriverSender, WorkItem};
use crate::error::AppError;
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::oneshot;

/// Cloneable handle to the persistent asyncio event loop.
///
/// Hot-path scheduling goes through the crossbeam channel — no GIL,
/// no Python object allocation. Driver threads consume items and
/// drive coroutines concurrently via `spawn_and_drive` / `resume_task`.
pub struct EventLoopHandle {
    /// Python event loop reference (diagnostics/tests).
    event_loop: Py<PyAny>,
    /// Sender side of the driver channel.
    driver_sender: DriverSender,
    /// Whether the event loop is still running.
    running: Arc<AtomicBool>,
}

impl Clone for EventLoopHandle {
    fn clone(&self) -> Self {
        Python::attach(|py| Self {
            event_loop: self.event_loop.clone_ref(py),
            driver_sender: self.driver_sender.clone(),
            running: Arc::clone(&self.running),
        })
    }
}

impl EventLoopHandle {
    /// Create a new handle with driver channel infrastructure.
    pub(crate) fn new(
        event_loop: Py<PyAny>,
        running: Arc<AtomicBool>,
        driver_sender: DriverSender,
    ) -> Self {
        Self {
            event_loop,
            driver_sender,
            running,
        }
    }

    /// Submit a Python coroutine to the running event loop.
    ///
    /// The coroutine runs on a driver thread with full asyncio context
    /// (BackgroundTasks, contextvars, get_running_loop).
    ///
    /// # Errors
    ///
    /// - `AppError::Internal` if the event loop is stopped or scheduling fails.
    pub async fn drive_coroutine(&self, coro: Py<PyAny>) -> Result<Py<PyAny>, AppError> {
        let rx = self.schedule_deferred(move |_py| Ok(coro))?;
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

    /// Defer all Python work to driver threads via the crossbeam channel.
    ///
    /// Hot path: no GIL acquisition. The builder closure is boxed and pushed
    /// to the channel. A driver thread picks it up and drives the coroutine.
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
        let t_push = trace.then(std::time::Instant::now);

        let (tx, rx) = oneshot::channel();
        let item = WorkItem {
            builder: Box::new(f),
            tx,
        };

        self.driver_sender
            .send_work(item)
            .map_err(|_| AppError::Internal("work queue closed".to_owned()))?;

        if let Some(t_push) = t_push {
            tracing::info!(
                target: "bench_trace",
                phase = "queue_push",
                push_us = t_push.elapsed().as_micros(),
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
    use pyo3::types::PyDict;

    #[tokio::test]
    async fn drive_trivial_coroutine() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();

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

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();

        // Coroutine that uses asyncio.sleep (requires running event loop).
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

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();

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

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();
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

    #[tokio::test]
    async fn drive_concurrent_async_coroutines() {
        // Schedule N coroutines that REQUIRE event loop I/O (asyncio.sleep).
        // Without GIL yielding, these deadlock — the event loop can't resolve
        // the sleep futures because the driver thread holds the GIL.
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();

        let n = 50;
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let h = handle.clone();
            handles.push(tokio::spawn(async move {
                let coro = Python::attach(|py| {
                    let code = std::ffi::CString::new(
                        "async def _t():\n    import asyncio\n    await asyncio.sleep(0)\n    return 'ok'\ncoro = _t()\n",
                    )
                    .unwrap();
                    let locals = PyDict::new(py);
                    py.run(&code, None, Some(&locals)).unwrap();
                    locals.get_item("coro").unwrap().unwrap().unbind()
                });
                // Timeout: if GIL starvation occurs, this hangs forever.
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    h.drive_coroutine(coro),
                )
                .await
                .unwrap() // timeout → GIL starvation
                .unwrap()
            }));
        }

        for jh in handles {
            let result = jh.await.unwrap();
            Python::attach(|py| {
                let val: String = result.extract(py).unwrap();
                assert_eq!(val, "ok");
            });
        }

        event_loop.stop();
    }

    #[test]
    fn handle_debug() {
        crate::with_py(|_py| {});
        let mut event_loop = EventLoop::start("asyncio").unwrap();
        let handle = event_loop.handle();
        let dbg = format!("{handle:?}");
        assert!(dbg.contains("EventLoopHandle"));
        event_loop.stop();
    }
}
