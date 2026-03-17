//! Event loop management for worker processes.
//!
//! Provides [`InlineEventLoop`] — a single-thread asyncio event loop that
//! runs dormant while the Rust scheduler drives coroutines inline on the
//! tokio thread.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use pyo3::prelude::*;

use crate::ffi::{CoroutineOps, FfiCoroutineOps};

use super::counters::{self, SchedulerCounters};
use super::driver::TaskOps;
use super::queue::ReadyQueue;

// ── Asyncio event loop utilities ─────────────────────────────────────────

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

// ── InlineEventLoop ──────────────────────────────────────────────────────

/// Single-thread event loop for worker processes.
///
/// Initializes the asyncio event loop as dormant (installed but not running
/// `run_forever()`). The Rust scheduler drives coroutines inline on the
/// tokio thread.
pub struct EventLoop {
    /// Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Coroutine stepping and classification operations.
    coroutine_ops: Arc<dyn CoroutineOps>,
    /// Per-worker ready queue for suspended tasks.
    ready_queue: Arc<ReadyQueue>,
    /// Cached `loop.call_soon_threadsafe` bound method (thread-safe variant,
    /// needed since the asyncio loop runs on a dedicated thread).
    call_soon_threadsafe: Py<PyAny>,
    /// Cached Python callables for scheduler task lifecycle.
    task_ops: TaskOps,
    /// Notify for waking the drain task when ready queue has items.
    /// Held to keep the Arc alive for the spawned drain task.
    #[expect(dead_code, reason = "Arc kept alive for spawned drain task")]
    drain_notify: Arc<tokio::sync::Notify>,
    /// Dedicated OS thread running `loop.run_forever()`.
    asyncio_thread: Mutex<Option<JoinHandle<()>>>,
}

impl EventLoop {
    /// Initialize the inline event loop on the current thread.
    ///
    /// Sets up the asyncio event loop in "dormant" mode — installed and
    /// registered as the running loop, but without calling `run_forever()`.
    /// This makes `asyncio.get_running_loop()` work for libraries that need it.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization fails.
    pub fn init(py: Python<'_>, loop_policy: &str) -> Result<Self, String> {
        // 1. Install loop policy (uvloop or asyncio).
        install_loop_policy(py, loop_policy);

        // 2. Create asyncio event loop.
        let event_loop = create_event_loop(py).map_err(|e| format!("create_event_loop: {e}"))?;

        // 3. Set as current event loop.
        let asyncio = py
            .import(c"asyncio")
            .map_err(|e| format!("import asyncio: {e}"))?;
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .map_err(|e| format!("set_event_loop: {e}"))?;

        // 4. Mark as running loop WITHOUT calling run_forever().
        // This makes asyncio.get_running_loop() work for libraries
        // (Starlette middleware, DB drivers, etc.).
        let events = py
            .import(c"asyncio.events")
            .map_err(|e| format!("import asyncio.events: {e}"))?;
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .map_err(|e| format!("_set_running_loop: {e}"))?;
        tracing::info!("inline event loop: _set_running_loop installed (dormant mode)");

        // 5. Set eager task factory (Python 3.12+).
        if let Ok(eager_factory) = asyncio.getattr(c"eager_task_factory") {
            match event_loop.call_method1(c"set_task_factory", (eager_factory,)) {
                Ok(_) => tracing::info!("eager task factory enabled (Python 3.12+)"),
                Err(e) => tracing::debug!("eager task factory not available: {e}"),
            }
        }

        // 5b. Cache _enter_task / _leave_task / _SchedulerTask for task lifecycle.
        let tasks_mod = py
            .import(c"asyncio.tasks")
            .map_err(|e| format!("import asyncio.tasks: {e}"))?;
        let enter_task = tasks_mod
            .getattr(c"_enter_task")
            .map_err(|e| format!("missing _enter_task: {e}"))?
            .unbind();
        let leave_task = tasks_mod
            .getattr(c"_leave_task")
            .map_err(|e| format!("missing _leave_task: {e}"))?
            .unbind();
        let task_mod = py
            .import(c"apx._task")
            .map_err(|e| format!("import apx._task: {e}"))?;
        let scheduler_task_cls = task_mod
            .getattr(c"_SchedulerTask")
            .map_err(|e| format!("missing _SchedulerTask: {e}"))?
            .unbind();
        let task_ops = TaskOps {
            enter_task,
            leave_task,
            scheduler_task_cls,
        };

        // 6. Resolve coroutine ops (FFI implementation).
        let coroutine_ops: Arc<dyn CoroutineOps> = Arc::new(
            FfiCoroutineOps::resolve(py).map_err(|e| format!("FfiCoroutineOps::resolve: {e}"))?,
        );

        // 7. Create ready queue.
        let ready_queue = Arc::new(ReadyQueue::new());

        // 7b. Initialize scheduler counters.
        let scheduler_counters = Arc::new(SchedulerCounters::new());
        counters::init(Arc::clone(&scheduler_counters));

        // 8. Cache call_soon_threadsafe (thread-safe variant, needed since
        // the asyncio loop now runs on a dedicated thread).
        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .map_err(|e| format!("missing call_soon_threadsafe: {e}"))?
            .unbind();

        // 9. Install tokio handle for scheduler primitives.
        let tokio_handle = tokio::runtime::Handle::try_current().ok();
        install_rust_scheduler(py, tokio_handle).map_err(|e| format!("scheduler install: {e}"))?;

        // 10. Create notify for drain task wake.
        let drain_notify = Arc::new(tokio::sync::Notify::new());

        // 11. Set notify-based wake on the ready queue.
        ready_queue.set_notify_wake(Arc::clone(&drain_notify));

        // 12. Spawn the drain task on the current-thread tokio runtime.
        let rq = Arc::clone(&ready_queue);
        let ct = Arc::clone(&coroutine_ops);
        let cs = call_soon_threadsafe.clone_ref(py);
        let notify = Arc::clone(&drain_notify);
        let drain_enter = task_ops.enter_task.clone_ref(py);
        let drain_leave = task_ops.leave_task.clone_ref(py);
        let drain_cls = task_ops.scheduler_task_cls.clone_ref(py);
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                Python::attach(|py| {
                    let drain_ops = TaskOps {
                        enter_task: drain_enter.clone_ref(py),
                        leave_task: drain_leave.clone_ref(py),
                        scheduler_task_cls: drain_cls.clone_ref(py),
                    };
                    rq.drain(py, &ct, &cs, &rq, &drain_ops);
                });
            }
        });

        // 13. Spawn dedicated asyncio thread running run_forever().
        // The loop processes call_soon_threadsafe callbacks naturally when
        // woken by thread pool completions (uv_async_send / self-pipe).
        let el_for_thread = event_loop.clone().unbind();
        let asyncio_thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    if let Err(e) = el.call_method0(c"run_forever") {
                        tracing::error!(error = %e, "asyncio thread: run_forever failed");
                    }
                });
            })
            .map_err(|e| format!("spawn asyncio thread: {e}"))?;

        tracing::info!("event loop initialized (dedicated asyncio thread)");

        Ok(Self {
            event_loop: event_loop.unbind(),
            coroutine_ops,
            ready_queue,
            call_soon_threadsafe,
            task_ops,
            drain_notify,
            asyncio_thread: Mutex::new(Some(asyncio_thread)),
        })
    }

    /// Get the coroutine operations.
    pub fn coroutine_ops(&self) -> &Arc<dyn CoroutineOps> {
        &self.coroutine_ops
    }

    /// Get the ready queue.
    pub fn ready_queue(&self) -> &Arc<ReadyQueue> {
        &self.ready_queue
    }

    /// Get the cached `call_soon_threadsafe` method.
    pub fn call_soon_threadsafe(&self) -> &Py<PyAny> {
        &self.call_soon_threadsafe
    }

    /// Get the cached task lifecycle operations.
    pub fn task_ops(&self) -> &TaskOps {
        &self.task_ops
    }

    /// Shut down the event loop.
    ///
    /// 1. Stops the asyncio loop (wakes `run_forever` via `call_soon_threadsafe`).
    /// 2. Joins the dedicated asyncio thread.
    /// 3. Cancels pending tasks and closes the loop.
    pub fn shutdown(&self) {
        // 1. Stop the asyncio loop (wakes run_forever via call_soon_threadsafe).
        Python::attach(|py| {
            let el = self.event_loop.bind(py);
            if let Ok(stop) = el.getattr(c"stop") {
                let _ = el.call_method1(c"call_soon_threadsafe", (stop,));
            }
        });

        // 2. Join the asyncio thread (run_forever returns after stop).
        let handle = self
            .asyncio_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = handle
            && let Err(e) = h.join()
        {
            tracing::warn!("asyncio thread panicked: {e:?}");
        }

        // 3. Clean up (cancel tasks, close loop).
        Python::attach(|py| {
            let el = self.event_loop.bind(py);
            if let Ok(events) = py.import(c"asyncio.events") {
                let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            }
            cancel_pending_tasks(py, el);
            let _ = el.call_method0(c"close");
        });
    }
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineEventLoop").finish_non_exhaustive()
    }
}
