//! Persistent asyncio event loop on a dedicated Python thread.
//!
//! One `EventLoop` per worker. The dedicated thread runs `run_forever()`,
//! which drives all handler coroutines, `BackgroundTasks`, and `contextvars`
//! natively. Other threads submit work via [`super::handle::EventLoopHandle`].

use super::handle::EventLoopHandle;
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── LoopPolicy ───────────────────────────────────────────────────────────

/// Event loop implementation policy.
///
/// Determines which Python event loop implementation to use for the
/// persistent worker loop.
///
/// Designed for future extension — a `RustNative` variant can replace
/// the Python event loop with a Rust-driven coroutine executor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopPolicy {
    /// Try uvloop first, fall back to default asyncio.
    #[default]
    Auto,
    /// Force uvloop (fails if not installed).
    UvLoop,
    /// Force CPython's default asyncio event loop.
    Asyncio,
}

impl std::fmt::Display for LoopPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::UvLoop => f.write_str("uvloop"),
            Self::Asyncio => f.write_str("asyncio"),
        }
    }
}

/// Create a Python event loop according to the given policy.
fn create_event_loop(py: Python<'_>, policy: LoopPolicy) -> PyResult<Bound<'_, PyAny>> {
    match policy {
        LoopPolicy::Auto => {
            if let Ok(uvloop) = py.import(c"uvloop") {
                tracing::info!("using uvloop event loop");
                return uvloop.call_method0(c"new_event_loop");
            }
            tracing::info!("uvloop not available, using default asyncio");
            py.import(c"asyncio")?.call_method0(c"new_event_loop")
        }
        LoopPolicy::UvLoop => {
            let uvloop = py.import(c"uvloop")?;
            tracing::info!("using uvloop event loop");
            uvloop.call_method0(c"new_event_loop")
        }
        LoopPolicy::Asyncio => {
            tracing::info!("using default asyncio event loop");
            py.import(c"asyncio")?.call_method0(c"new_event_loop")
        }
    }
}

/// Cancel all pending asyncio tasks and run them to completion.
///
/// Without this step, `loop.close()` leaves live tasks whose cleanup
/// callbacks call `call_soon_threadsafe` on the already-closed loop,
/// producing `RuntimeError: Event loop is closed` on stderr.
fn cancel_pending_tasks(py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    let Ok(asyncio) = py.import(c"asyncio") else {
        return;
    };
    let Ok(tasks) = asyncio.call_method1(c"all_tasks", (event_loop,)) else {
        return;
    };
    let Ok(task_iter) = tasks.try_iter() else {
        return;
    };
    for task in task_iter.flatten() {
        let _ = task.call_method0(c"cancel");
    }
    // Drive cancelled tasks so their CancelledError propagates.
    let Ok(gather) = asyncio.call_method(c"gather", (&tasks,), Some(&gather_kwargs(py))) else {
        return;
    };
    let _ = event_loop.call_method1(c"run_until_complete", (gather,));
}

/// Build `return_exceptions=True` kwargs for `asyncio.gather`.
fn gather_kwargs(py: Python<'_>) -> Bound<'_, pyo3::types::PyDict> {
    let kwargs = pyo3::types::PyDict::new(py);
    let _ = kwargs.set_item("return_exceptions", true);
    kwargs
}

/// Persistent asyncio event loop running on a dedicated OS thread.
///
/// Created once per worker via [`EventLoop::start`]. The loop runs
/// `run_forever()` on its thread, processing coroutines scheduled via
/// [`EventLoopHandle::drive_coroutine`].
pub struct EventLoop {
    /// Reference to the Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Handle to the dedicated Python thread (joined on shutdown).
    thread: Option<std::thread::JoinHandle<()>>,
    /// Whether the loop is still running (guards `call_soon_threadsafe`).
    running: Arc<AtomicBool>,
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoop")
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl EventLoop {
    /// Start with [`LoopPolicy::Auto`] (uvloop if available, else asyncio).
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization or event loop creation fails.
    pub fn start() -> Result<Self, String> {
        Self::start_with(LoopPolicy::default())
    }

    /// Start a persistent event loop with an explicit [`LoopPolicy`].
    ///
    /// Returns the `EventLoop` with the loop running `run_forever()`.
    /// The caller must call [`stop`] before dropping to cleanly shut down.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization or event loop creation fails.
    pub fn start_with(policy: LoopPolicy) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let result = (|| -> PyResult<Py<PyAny>> {
                        let event_loop = create_event_loop(py, policy)?;
                        let asyncio = py.import(c"asyncio")?;
                        asyncio.call_method1(c"set_event_loop", (&event_loop,))?;
                        Ok(event_loop.unbind())
                    })();

                    match result {
                        Ok(event_loop) => {
                            let _ = tx.send(Ok(event_loop.clone_ref(py)));

                            let loop_bound = event_loop.bind(py);
                            if let Err(e) = loop_bound.call_method0(c"run_forever") {
                                tracing::error!(error = %e, "run_forever failed");
                            }
                            running_clone.store(false, Ordering::Release);

                            let _ = Self::close_loop(py, loop_bound);
                        }
                        Err(e) => {
                            let _ = tx.send(Err(format!("event loop creation failed: {e}")));
                        }
                    }
                });
            })
            .map_err(|e| format!("failed to spawn asyncio thread: {e}"))?;

        let event_loop = rx
            .recv()
            .map_err(|_| "asyncio thread exited before sending loop".to_owned())??;

        Ok(Self {
            event_loop,
            thread: Some(thread),
            running,
        })
    }

    /// Get a cloneable handle for submitting work to this event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if event loop method caching fails.
    pub fn handle(&self) -> Result<EventLoopHandle, String> {
        Python::attach(|py| {
            EventLoopHandle::new(self.event_loop.clone_ref(py), Arc::clone(&self.running))
        })
    }

    /// Get a reference to the underlying Python event loop object.
    pub fn event_loop_ref(&self) -> &Py<PyAny> {
        &self.event_loop
    }

    /// Stop the event loop and join the dedicated thread.
    ///
    /// Sends `loop.stop()` via `call_soon_threadsafe`, then joins the thread.
    /// Safe to call multiple times (no-op after the first call).
    pub fn stop(&mut self) {
        if !self.running.load(Ordering::Acquire) {
            // Already stopped.
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            return;
        }

        self.running.store(false, Ordering::Release);

        // Schedule loop.stop() on the event loop thread.
        Python::attach(|py| {
            let stop_fn = self.event_loop.getattr(py, "stop");
            match stop_fn {
                Ok(stop) => {
                    let _ = self
                        .event_loop
                        .call_method1(py, "call_soon_threadsafe", (stop,));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to get loop.stop");
                }
            }
        });

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Cancel pending tasks, drain generators, and close the event loop.
    ///
    /// Follows the standard asyncio shutdown sequence:
    /// 1. Cancel all pending tasks and await their cancellation.
    /// 2. Shut down async generators.
    /// 3. Shut down the default executor.
    /// 4. Close the loop.
    fn close_loop(py: Python<'_>, event_loop: &Bound<'_, PyAny>) -> PyResult<()> {
        cancel_pending_tasks(py, event_loop);

        let shutdown_gens = event_loop.call_method0(c"shutdown_asyncgens")?;
        let _ = event_loop.call_method1(c"run_until_complete", (shutdown_gens,));

        let shutdown_exec = event_loop.call_method0(c"shutdown_default_executor")?;
        let _ = event_loop.call_method1(c"run_until_complete", (shutdown_exec,));

        event_loop.call_method0(c"close")?;

        // Replace call_soon_threadsafe with a no-op so late pyo3_async_runtimes
        // cleanup callbacks (tokio tasks outliving the loop) don't produce
        // `RuntimeError: Event loop is closed` on stderr.
        let noop = py.eval(c"lambda *a, **kw: None", None, None)?;
        event_loop.setattr(c"call_soon_threadsafe", noop)?;

        Ok(())
    }
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn start_and_stop() {
        // Initialize Python if needed (test helper).
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        assert!(event_loop.running.load(Ordering::Acquire));

        let handle = event_loop.handle().unwrap();
        // Verify handle is valid (loop ref is not None).
        Python::attach(|py| {
            assert!(!handle.event_loop().bind(py).is_none());
        });

        event_loop.stop();
        assert!(!event_loop.running.load(Ordering::Acquire));
    }

    #[test]
    fn double_stop_is_safe() {
        crate::with_py(|_py| {});

        let mut event_loop = EventLoop::start().unwrap();
        event_loop.stop();
        event_loop.stop(); // Should not panic.
    }
}
