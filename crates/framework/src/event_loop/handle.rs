//! Cloneable handle for submitting coroutines to the persistent event loop.
//!
//! [`EventLoopHandle`] is the main interface used by dispatch code. It's
//! cheaply cloneable and safe to use from any Tokio task.
//!
//! The hot path (`schedule_deferred`) pushes work items to an MPSC queue
//! with zero GIL acquisition. The event loop thread's [`QueueDrainer`]
//! processes them in batch.

use super::queue::{QueueItem, WorkItem};
use super::wake::WakeStrategy;
use crate::error::AppError;
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

/// Cloneable handle to the persistent asyncio event loop.
///
/// Hot-path scheduling goes through the MPSC queue — no GIL, no Python
/// object allocation. The event loop thread builds scope dicts and drives
/// coroutines via the [`super::queue::QueueDrainer`].
pub struct EventLoopHandle {
    /// Python event loop reference (diagnostics/tests).
    event_loop: Py<PyAny>,
    /// Producer side of the work queue (lock-free push).
    queue_tx: mpsc::UnboundedSender<QueueItem>,
    /// Shared flag: `true` means drainer is sleeping and needs a wake.
    needs_wake: Arc<AtomicBool>,
    /// Wake strategy (pipe on Unix, GIL fallback on Windows).
    wake: Arc<WakeStrategy>,
    /// Whether the event loop is still running.
    running: Arc<AtomicBool>,
}

impl Clone for EventLoopHandle {
    fn clone(&self) -> Self {
        Python::attach(|py| Self {
            event_loop: self.event_loop.clone_ref(py),
            queue_tx: self.queue_tx.clone(),
            needs_wake: Arc::clone(&self.needs_wake),
            wake: Arc::clone(&self.wake),
            running: Arc::clone(&self.running),
        })
    }
}

impl EventLoopHandle {
    /// Create a new handle with queue and wake infrastructure.
    pub(crate) fn new(
        event_loop: Py<PyAny>,
        running: Arc<AtomicBool>,
        queue_tx: mpsc::UnboundedSender<QueueItem>,
        needs_wake: Arc<AtomicBool>,
        wake: Arc<WakeStrategy>,
    ) -> Self {
        Self {
            event_loop,
            queue_tx,
            needs_wake,
            wake,
            running,
        }
    }

    /// Submit a Python coroutine to the running event loop.
    ///
    /// The coroutine runs on the event loop thread with full asyncio context
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

    /// Defer all Python work to the event loop thread via the MPSC queue.
    ///
    /// Hot path: no GIL acquisition. The builder closure is boxed and pushed
    /// to the queue. If the drainer is sleeping, a pipe write (or
    /// `call_soon_threadsafe` fallback) wakes it.
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
        let item = QueueItem::Work(WorkItem {
            builder: Box::new(f),
            tx,
        });

        self.queue_tx
            .send(item)
            .map_err(|_| AppError::Internal("work queue closed".to_owned()))?;

        if let Some(t_push) = t_push {
            tracing::info!(
                target: "bench_trace",
                phase = "queue_push",
                push_us = t_push.elapsed().as_micros(),
            );
        }

        self.wake_if_sleeping();

        Ok(rx)
    }

    /// Wake the drainer if it's sleeping (idle→active transition).
    ///
    /// Uses `swap(false, AcqRel)` — the Acquire synchronizes with the
    /// drainer's Release store, ensuring our queue push is visible.
    fn wake_if_sleeping(&self) {
        let was_sleeping = self.needs_wake.swap(false, Ordering::AcqRel);
        if was_sleeping {
            self.wake.wake();
        }
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
