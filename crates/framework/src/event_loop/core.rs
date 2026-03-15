//! Persistent asyncio event loop on a dedicated Python thread.
//!
//! One `EventLoop` per worker. The dedicated thread runs `run_forever()`,
//! serving as a background I/O reactor (socket ops, DNS, asyncio.Future
//! resolution). Coroutine driving is distributed across N driver threads
//! in the [`DriverPool`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;

use super::handle::EventLoopHandle;
use crate::driver_pool::{DriverPool, SharedDriverState};
use crate::scheduler::driver::CachedTypes;
use crate::scheduler::queue::ReadyQueue;

/// Install the event loop policy (uvloop or asyncio) before creating the loop.
///
/// Must be called before `asyncio.new_event_loop()` so the factory picks up
/// the right policy.
fn install_loop_policy(py: Python<'_>, policy: &str) {
    if policy == "uvloop" {
        match py.import(c"uvloop") {
            Ok(uvloop) => {
                let Ok(asyncio) = py.import(c"asyncio") else {
                    tracing::error!("failed to import asyncio for uvloop policy install");
                    return;
                };
                let Ok(policy_obj) = uvloop.call_method0(c"EventLoopPolicy") else {
                    tracing::error!("uvloop.EventLoopPolicy() call failed");
                    return;
                };
                if let Err(e) = asyncio.call_method1(c"set_event_loop_policy", (policy_obj,)) {
                    tracing::error!(error = %e, "asyncio.set_event_loop_policy() failed");
                    return;
                }
                tracing::info!("installed uvloop event loop policy");
            }
            Err(e) => {
                tracing::warn!(error = %e, "uvloop not available, falling back to asyncio");
            }
        }
    } else {
        tracing::info!(policy, "using asyncio event loop policy");
    }
}

/// Create an asyncio event loop as the I/O reactor (socket ops, DNS).
///
/// The Rust scheduler drives all coroutine scheduling; asyncio only
/// resolves `asyncio.Future`s from network I/O libraries.
fn create_event_loop(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    tracing::info!("creating asyncio I/O reactor");
    py.import(c"asyncio")?.call_method0(c"new_event_loop")
}

/// Initialize Rust scheduler state on the current event loop thread.
///
/// Stores the tokio runtime handle in a thread-local. Does NOT monkeypatch
/// asyncio — native asyncio coroutines are handled by the driver's
/// `WaitingOnAsyncioFuture` path.
fn install_rust_scheduler(
    _py: Python<'_>,
    tokio_handle: Option<tokio::runtime::Handle>,
) -> PyResult<()> {
    if let Some(handle) = tokio_handle {
        crate::scheduler::set_tokio_handle(handle);
    }
    tracing::info!("rust scheduler initialized (no asyncio monkeypatching)");
    Ok(())
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

/// Default number of driver threads in the pool.
const DEFAULT_DRIVER_THREADS: usize = 1;

/// Persistent asyncio event loop running on a dedicated OS thread.
///
/// Created once per worker via [`EventLoop::start`]. The event loop thread
/// runs `run_forever()` as a background I/O reactor. Coroutine driving is
/// handled by the [`DriverPool`].
pub struct EventLoop {
    /// Reference to the Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Handle to the dedicated Python thread (joined on shutdown).
    thread: Option<std::thread::JoinHandle<()>>,
    /// Whether the loop is still running (guards `call_soon_threadsafe`).
    running: Arc<AtomicBool>,
    /// Pool of driver threads for coroutine execution.
    driver_pool: DriverPool,
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoop")
            .field("running", &self.running.load(Ordering::Relaxed))
            .field("driver_pool", &self.driver_pool)
            .finish_non_exhaustive()
    }
}

impl EventLoop {
    /// Start a persistent event loop with the Rust scheduler.
    ///
    /// `loop_policy` is `"uvloop"` or `"asyncio"`. When `"uvloop"`, the uvloop
    /// event loop policy is installed before creating the asyncio loop.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization or event loop creation fails.
    pub fn start(loop_policy: &str) -> Result<Self, String> {
        let (startup_tx, startup_rx) = std::sync::mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        // Capture the tokio runtime handle for scheduler primitives.
        let tokio_handle = tokio::runtime::Handle::try_current().ok();

        let loop_policy_owned = loop_policy.to_owned();
        let thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    // Install loop policy before creating the event loop.
                    install_loop_policy(py, &loop_policy_owned);

                    let result = Self::init_event_loop_thread(py, tokio_handle);

                    match result {
                        Ok((event_loop, driver_pool)) => {
                            let _ = startup_tx.send(Ok((event_loop.clone_ref(py), driver_pool)));

                            let loop_bound = event_loop.bind(py);
                            if let Err(e) = loop_bound.call_method0(c"run_forever") {
                                tracing::error!(error = %e, "run_forever failed");
                            }
                            running_clone.store(false, Ordering::Release);

                            let _ = Self::close_loop(py, loop_bound);
                        }
                        Err(e) => {
                            let _ =
                                startup_tx.send(Err(format!("event loop creation failed: {e}")));
                        }
                    }
                });
            })
            .map_err(|e| format!("failed to spawn asyncio thread: {e}"))?;

        let (event_loop, driver_pool) = startup_rx
            .recv()
            .map_err(|_| "asyncio thread exited before sending loop".to_owned())??;

        Ok(Self {
            event_loop,
            thread: Some(thread),
            running,
            driver_pool,
        })
    }

    /// Initialize the event loop, create the [`DriverPool`], and return
    /// the event loop ref + pool.
    fn init_event_loop_thread(
        py: Python<'_>,
        tokio_handle: Option<tokio::runtime::Handle>,
    ) -> Result<(Py<PyAny>, DriverPool), String> {
        let event_loop = create_event_loop(py).map_err(|e| format!("create_event_loop: {e}"))?;
        let asyncio = py
            .import(c"asyncio")
            .map_err(|e| format!("import asyncio: {e}"))?;
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .map_err(|e| format!("set_event_loop: {e}"))?;

        // Python 3.12+ eager task factory — runs first coroutine step inline
        // during create_task, eliminating one event loop round-trip for handlers
        // that complete synchronously.
        if let Ok(eager_factory) = asyncio.getattr(c"eager_task_factory") {
            match event_loop.call_method1(c"set_task_factory", (eager_factory,)) {
                Ok(_) => tracing::info!("eager task factory enabled (Python 3.12+)"),
                Err(e) => tracing::debug!("eager task factory not available: {e}"),
            }
        }

        // Resolve cached types and create shared state.
        let cached_types =
            Arc::new(CachedTypes::resolve(py).map_err(|e| format!("CachedTypes::resolve: {e}"))?);
        let ready_queue = Arc::new(ReadyQueue::new());

        // Resolve call_soon_threadsafe for driver threads.
        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .map_err(|e| format!("missing call_soon_threadsafe: {e}"))?
            .unbind();

        // Install the tokio handle for scheduler primitives.
        install_rust_scheduler(py, tokio_handle.clone())
            .map_err(|e| format!("scheduler install: {e}"))?;

        // Create the driver pool.
        let shared = SharedDriverState {
            cached_types: Arc::clone(&cached_types),
            event_loop_ref: event_loop.clone().unbind(),
            call_soon_threadsafe,
            ready_queue: Arc::clone(&ready_queue),
            tokio_handle,
        };
        let driver_pool = DriverPool::start(DEFAULT_DRIVER_THREADS, shared);

        // Wire up the ready queue to send resumed tasks to the driver pool.
        ready_queue.set_wake(driver_pool.sender());

        Ok((event_loop.unbind(), driver_pool))
    }

    /// Get a cloneable handle for submitting work to this event loop.
    pub fn handle(&self) -> EventLoopHandle {
        Python::attach(|py| {
            EventLoopHandle::new(
                self.event_loop.clone_ref(py),
                Arc::clone(&self.running),
                self.driver_pool.sender(),
            )
        })
    }

    /// Get a reference to the underlying Python event loop object.
    pub fn event_loop_ref(&self) -> &Py<PyAny> {
        &self.event_loop
    }

    /// Add driver threads at runtime (dynamic scale-up).
    pub fn add_drivers(&self, n: usize) -> Vec<usize> {
        self.driver_pool.add_threads(n)
    }

    /// Remove driver threads at runtime (dynamic scale-down).
    pub fn remove_drivers(&self, n: usize) {
        self.driver_pool.remove_threads(n);
    }

    /// Current number of active driver threads.
    pub fn driver_count(&self) -> usize {
        self.driver_pool.thread_count()
    }

    /// Stop the event loop and join the dedicated thread.
    ///
    /// Stops the driver pool first, then sends `loop.stop()` via
    /// `call_soon_threadsafe`, then joins the thread.
    /// Safe to call multiple times (no-op after the first call).
    pub fn stop(&mut self) {
        if !self.running.load(Ordering::Acquire) {
            // Already stopped.
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            return;
        }

        // Stop the driver pool first so in-flight coroutines drain.
        self.driver_pool.stop();

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

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        assert!(event_loop.running.load(Ordering::Acquire));
        assert_eq!(event_loop.driver_count(), DEFAULT_DRIVER_THREADS);

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

        let mut event_loop = EventLoop::start("asyncio").unwrap();
        event_loop.stop();
        event_loop.stop(); // Should not panic.
    }
}
