//! Persistent asyncio event loop on a dedicated Python thread.
//!
//! One `EventLoop` per worker. The dedicated thread runs `run_forever()`,
//! which drives all handler coroutines, `BackgroundTasks`, and `contextvars`
//! natively. Other threads submit work via [`super::handle::EventLoopHandle`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use tokio::sync::mpsc;

use super::handle::EventLoopHandle;
use super::queue::{QueueDrainer, QueueItem, SchedulerState};
use crate::scheduler::driver::CachedTypes;
use crate::scheduler::queue::ReadyQueue;

// ── LoopPolicy ───────────────────────────────────────────────────────────

/// Event loop implementation policy.
///
/// Determines which Python event loop implementation to use for the
/// persistent worker loop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoopPolicy {
    /// Try uvloop first, fall back to default asyncio.
    Auto,
    /// Force uvloop (fails if not installed).
    UvLoop,
    /// Force CPython's default asyncio event loop.
    Asyncio,
    /// Rust-driven scheduler with asyncio fallback.
    #[default]
    RustNative,
}

impl std::fmt::Display for LoopPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::UvLoop => f.write_str("uvloop"),
            Self::Asyncio => f.write_str("asyncio"),
            Self::RustNative => f.write_str("rust-native"),
        }
    }
}

impl LoopPolicy {
    /// Create a policy from the `APX_SCHEDULER` environment variable.
    ///
    /// - `APX_SCHEDULER=auto` -> `Auto`
    /// - `APX_SCHEDULER=uvloop` -> `UvLoop`
    /// - `APX_SCHEDULER=asyncio` -> `Asyncio`
    /// - Default (unset) -> `RustNative`
    pub fn from_env() -> Self {
        match std::env::var("APX_SCHEDULER").as_deref() {
            Ok("auto") => Self::Auto,
            Ok("uvloop") => Self::UvLoop,
            Ok("asyncio") => Self::Asyncio,
            _ => Self::RustNative,
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
        LoopPolicy::RustNative => {
            // RustNative still needs an asyncio event loop as fallback.
            // Use the default asyncio loop (not uvloop) for predictability.
            tracing::info!("using rust-native scheduler with asyncio fallback");
            py.import(c"asyncio")?.call_method0(c"new_event_loop")
        }
    }
}

/// Initialize Rust scheduler state on the current event loop thread.
///
/// Stores the tokio runtime handle in a thread-local for use by
/// scheduler primitives (Timer, spawn_blocking). Does NOT monkeypatch
/// asyncio — native asyncio coroutines are handled by the driver's
/// `WaitingOnAsyncioFuture` / `FallbackToAsyncio` paths.
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
    /// Producer side of the work queue.
    queue_tx: mpsc::UnboundedSender<QueueItem>,
    /// Shared flag: `true` means drainer is sleeping and needs a wake.
    needs_wake: Arc<AtomicBool>,
    /// Python reference to the [`QueueDrainer`] singleton.
    drainer_ref: Py<PyAny>,
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
        Self::start_with(LoopPolicy::from_env())
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
        let (startup_tx, startup_rx) = std::sync::mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        // Create the work queue before spawning — rx moves into the thread.
        let (queue_tx, queue_rx) = mpsc::unbounded_channel::<QueueItem>();
        let needs_wake = Arc::new(AtomicBool::new(false));
        let needs_wake_clone = Arc::clone(&needs_wake);

        // Capture the tokio runtime handle for scheduler primitives (Timer,
        // spawn_blocking). Always captured — the anyio backend uses these
        // regardless of whether the Rust coroutine driver is active.
        let tokio_handle = tokio::runtime::Handle::try_current().ok();

        let thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let result = Self::init_event_loop_thread(
                        py,
                        policy,
                        queue_rx,
                        needs_wake_clone,
                        tokio_handle,
                    );

                    match result {
                        Ok((event_loop, drainer_ref)) => {
                            let _ = startup_tx.send(Ok((event_loop.clone_ref(py), drainer_ref)));

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

        let (event_loop, drainer_ref) = startup_rx
            .recv()
            .map_err(|_| "asyncio thread exited before sending loop".to_owned())??;

        Ok(Self {
            event_loop,
            thread: Some(thread),
            running,
            queue_tx,
            needs_wake,
            drainer_ref,
        })
    }

    /// Initialize the event loop, install the [`QueueDrainer`], and return both.
    fn init_event_loop_thread(
        py: Python<'_>,
        policy: LoopPolicy,
        queue_rx: mpsc::UnboundedReceiver<QueueItem>,
        needs_wake: Arc<AtomicBool>,
        tokio_handle: Option<tokio::runtime::Handle>,
    ) -> Result<(Py<PyAny>, Py<PyAny>), String> {
        let event_loop =
            create_event_loop(py, policy).map_err(|e| format!("create_event_loop: {e}"))?;
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

        let drainer_ref = Self::install_drainer(py, &event_loop, queue_rx, needs_wake, policy)?;

        // Always install the tokio handle for scheduler primitives (Timer,
        // BlockingTask). The anyio backend uses these even without the Rust
        // coroutine driver.
        install_rust_scheduler(py, tokio_handle).map_err(|e| format!("scheduler install: {e}"))?;

        Ok((event_loop.unbind(), drainer_ref))
    }

    /// Create and install the [`QueueDrainer`] on the event loop.
    fn install_drainer(
        py: Python<'_>,
        event_loop: &Bound<'_, PyAny>,
        queue_rx: mpsc::UnboundedReceiver<QueueItem>,
        needs_wake: Arc<AtomicBool>,
        policy: LoopPolicy,
    ) -> Result<Py<PyAny>, String> {
        let create_task = event_loop
            .getattr(c"create_task")
            .map_err(|e| format!("missing create_task: {e}"))?
            .unbind();
        let call_soon = event_loop
            .getattr(c"call_soon")
            .map_err(|e| format!("missing call_soon: {e}"))?
            .unbind();

        let scheduler = if policy == LoopPolicy::RustNative {
            let cached_types = Arc::new(
                CachedTypes::resolve(py).map_err(|e| format!("CachedTypes::resolve: {e}"))?,
            );
            let asyncio = py
                .import(c"asyncio")
                .map_err(|e| format!("import asyncio: {e}"))?;
            let ensure_future = asyncio
                .getattr(c"ensure_future")
                .map_err(|e| format!("missing ensure_future: {e}"))?
                .unbind();
            Some(SchedulerState {
                cached_types,
                call_soon: call_soon.clone_ref(py),
                ensure_future,
                ready_queue: Arc::new(ReadyQueue::new()),
            })
        } else {
            None
        };

        // Clone the ready_queue Arc before scheduler moves into the drainer.
        let ready_queue_ref = scheduler.as_ref().map(|s| Arc::clone(&s.ready_queue));

        let drainer = QueueDrainer::new(
            queue_rx,
            create_task,
            call_soon.clone_ref(py),
            Arc::clone(&needs_wake),
            scheduler,
        );
        let drainer_obj =
            Py::new(py, drainer).map_err(|e| format!("QueueDrainer allocation: {e}"))?;

        // Set self_ref for call_soon(self) rescheduling.
        drainer_obj
            .borrow_mut(py)
            .set_self_ref(drainer_obj.clone_ref(py).into_any());

        // Set wake state on the ready queue so push() can reschedule
        // the drainer when it is sleeping.
        if let Some(ref rq) = ready_queue_ref {
            rq.set_wake(needs_wake, call_soon, drainer_obj.clone_ref(py).into_any());
        }

        // Install initial callback so the drainer starts processing.
        event_loop
            .call_method1(c"call_soon", (&drainer_obj,))
            .map_err(|e| format!("initial call_soon: {e}"))?;

        Ok(drainer_obj.into_any())
    }

    /// Get a cloneable handle for submitting work to this event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if event loop method caching fails.
    pub fn handle(&self) -> Result<EventLoopHandle, String> {
        Python::attach(|py| {
            EventLoopHandle::new(
                self.event_loop.clone_ref(py),
                Arc::clone(&self.running),
                self.queue_tx.clone(),
                Arc::clone(&self.needs_wake),
                self.drainer_ref.clone_ref(py),
            )
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

    #[test]
    fn from_env_defaults_to_rust_native() {
        // Default policy is based on compile-time default; don't mutate env.
        assert_eq!(LoopPolicy::default(), LoopPolicy::RustNative);
    }

    #[test]
    fn loop_policy_display() {
        assert_eq!(LoopPolicy::Auto.to_string(), "auto");
        assert_eq!(LoopPolicy::UvLoop.to_string(), "uvloop");
        assert_eq!(LoopPolicy::Asyncio.to_string(), "asyncio");
        assert_eq!(LoopPolicy::RustNative.to_string(), "rust-native");
    }
}
