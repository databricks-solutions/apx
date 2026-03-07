//! Persistent asyncio event loop on a dedicated Python thread.
//!
//! One `EventLoop` per worker. The dedicated thread runs `run_forever()`,
//! which drives all handler coroutines, `BackgroundTasks`, and `contextvars`
//! natively. Other threads submit work via [`super::handle::EventLoopHandle`].

use super::handle::EventLoopHandle;
use pyo3::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Start a persistent event loop on a new dedicated thread.
    ///
    /// Returns the `EventLoop` with the loop running `run_forever()`.
    /// The caller must call [`stop`] before dropping to cleanly shut down.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization or event loop creation fails.
    pub fn start() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        let thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                // This thread owns the asyncio event loop for its lifetime.
                // Python::attach acquires the GIL on this thread.
                Python::attach(|py| {
                    let result = (|| -> PyResult<Py<PyAny>> {
                        let asyncio = py.import(c"asyncio")?;
                        let event_loop = asyncio.call_method0(c"new_event_loop")?;
                        asyncio.call_method1(c"set_event_loop", (&event_loop,))?;
                        Ok(event_loop.unbind())
                    })();

                    match result {
                        Ok(event_loop) => {
                            // Send the loop reference back to the calling thread.
                            let _ = tx.send(Ok(event_loop.clone_ref(py)));

                            // run_forever() blocks until loop.stop() is called.
                            // During selector waits, the GIL is released, allowing
                            // other threads to acquire it for spawn_blocking work.
                            let loop_bound = event_loop.bind(py);
                            if let Err(e) = loop_bound.call_method0(c"run_forever") {
                                tracing::error!(error = %e, "asyncio run_forever failed");
                            }
                            running_clone.store(false, Ordering::Release);

                            // Cleanup after run_forever returns.
                            let _ = Self::close_loop(py, loop_bound);
                        }
                        Err(e) => {
                            let _ = tx.send(Err(format!("event loop creation failed: {e}")));
                        }
                    }
                });
            })
            .map_err(|e| format!("failed to spawn asyncio thread: {e}"))?;

        // Wait for the event loop reference from the new thread.
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
    pub fn handle(&self) -> EventLoopHandle {
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

    /// Drain async generators and close the event loop.
    fn close_loop(_py: Python<'_>, event_loop: &Bound<'_, PyAny>) -> PyResult<()> {
        let shutdown_coro = event_loop.call_method0(c"shutdown_asyncgens")?;
        let _ = event_loop.call_method1(c"run_until_complete", (shutdown_coro,));
        event_loop.call_method0(c"close")?;
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

        let handle = event_loop.handle();
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
