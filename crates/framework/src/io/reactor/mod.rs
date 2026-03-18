//! Asyncio event loop lifecycle — init, shutdown, task registration.
//!
//! The [`Reactor`] manages the Python asyncio event loop on a dedicated
//! thread. It owns the loop object, `call_soon_threadsafe`, and the
//! `_enter_task`/`_leave_task` registration API.

use std::sync::Mutex;
use std::thread::JoinHandle;

use pyo3::prelude::*;

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
    // Snapshot to list — all_tasks() returns a set backed by WeakSet.
    // Items can be GC'd during iteration, causing RuntimeError.
    let Ok(task_list) = pyo3::types::PyList::new(
        py,
        tasks
            .try_iter()
            .into_iter()
            .flatten()
            .flatten()
            .collect::<Vec<_>>(),
    ) else {
        return;
    };
    for task in task_list.iter() {
        let _ = task.call_method0(c"cancel");
    }
    // Drive cancelled tasks so their CancelledError propagates.
    let Ok(gather) = asyncio.call_method(c"gather", (&task_list,), Some(&gather_kwargs(py))) else {
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

/// Shut down all async generators — run their `aclose()` finalizers.
///
/// Without this, async generators abandoned without `aclose()` leak
/// their `finally` blocks. Matches `asyncio.run()` cleanup behavior.
fn shutdown_asyncgens(_py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    let Ok(coro) = event_loop.call_method0(c"shutdown_asyncgens") else {
        return;
    };
    if let Err(e) = event_loop.call_method1(c"run_until_complete", (&coro,)) {
        tracing::warn!(error = %e, "shutdown_asyncgens failed");
    }
}

/// Shut down the default thread pool executor with a timeout.
///
/// Uses a 5-second timeout to avoid the Ctrl+C deadlock documented
/// in CPython #111358. `asyncio.run()` uses 5 minutes — we use 5s
/// because our executor usage is minimal (DNS, file I/O).
fn shutdown_default_executor(py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    let Ok(coro) = event_loop.call_method0(c"shutdown_default_executor") else {
        return;
    };
    let Ok(asyncio) = py.import(c"asyncio") else {
        let _ = event_loop.call_method1(c"run_until_complete", (&coro,));
        return;
    };
    let Ok(wait_for) = asyncio.call_method1(c"wait_for", (&coro, 5.0)) else {
        let _ = event_loop.call_method1(c"run_until_complete", (&coro,));
        return;
    };
    if let Err(e) = event_loop.call_method1(c"run_until_complete", (&wait_for,)) {
        tracing::warn!(error = %e, "shutdown_default_executor failed");
    }
}

// ── TaskOps — cached Python callables for scheduler task lifecycle ────────

/// Cached Python callables for the scheduler task lifecycle.
///
/// Resolved once at worker init, passed by reference to avoid per-call imports.
pub struct TaskOps {
    pub enter_task: Py<PyAny>,
    pub leave_task: Py<PyAny>,
    pub scheduler_task_cls: Py<PyAny>,
    pub call_soon: Py<PyAny>,
    pub loop_obj: Py<PyAny>,
}

impl TaskOps {
    /// Clone all cached Python references under the current GIL.
    pub fn clone_ref(&self, py: Python<'_>) -> Self {
        Self {
            enter_task: self.enter_task.clone_ref(py),
            leave_task: self.leave_task.clone_ref(py),
            scheduler_task_cls: self.scheduler_task_cls.clone_ref(py),
            call_soon: self.call_soon.clone_ref(py),
            loop_obj: self.loop_obj.clone_ref(py),
        }
    }
}

impl std::fmt::Debug for TaskOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskOps").finish_non_exhaustive()
    }
}

// ── Scheduler task registration ──────────────────────────────────────────

/// Create a `_SchedulerTask` and register it as the current asyncio task.
///
/// Accepts the root coroutine directly so this function has no dependency on
/// `SchedulerTask`. The caller extracts the coro before calling.
pub fn create_scheduler_task(
    py: Python<'_>,
    coro: &Py<PyAny>,
    ops: &TaskOps,
) -> Option<(Py<PyAny>, Py<PyAny>)> {
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("loop", &ops.loop_obj).ok()?;
    let sched_task = ops
        .scheduler_task_cls
        .call(py, (coro,), Some(&kwargs))
        .ok()?;
    tracing::trace!("create_scheduler_task: created");
    Some((ops.loop_obj.clone_ref(py), sched_task))
}

// ── Reactor ──────────────────────────────────────────────────────────────

/// Asyncio event loop lifecycle manager.
///
/// Owns the Python asyncio event loop running on a dedicated OS thread,
/// the cached `call_soon_threadsafe` bound method, and the `TaskOps`
/// for `_enter_task`/`_leave_task` registration.
pub struct Reactor {
    /// Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Cached `loop.call_soon_threadsafe` bound method (thread-safe variant,
    /// needed since the asyncio loop runs on a dedicated thread).
    call_soon_threadsafe: Py<PyAny>,
    /// Cached Python callables for scheduler task lifecycle.
    task_ops: TaskOps,
    /// Dedicated OS thread running `loop.run_forever()`.
    asyncio_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Reactor {
    /// Initialize the reactor on the current thread.
    ///
    /// Sets up the asyncio event loop, marks it as running, enables eager
    /// task factory (Python 3.12+), caches task lifecycle callables, and
    /// spawns a dedicated OS thread running `run_forever()`.
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
        tracing::info!("reactor: _set_running_loop installed");

        // 5. Set eager task factory (Python 3.12+).
        if let Ok(eager_factory) = asyncio.getattr(c"eager_task_factory") {
            match event_loop.call_method1(c"set_task_factory", (eager_factory,)) {
                Ok(_) => tracing::info!("eager task factory enabled (Python 3.12+)"),
                Err(e) => tracing::debug!("eager task factory not available: {e}"),
            }
        }

        // 5b. Cache _enter_task / _leave_task / _SchedulerTask for task lifecycle.
        //
        // -- Private API: _enter_task / _leave_task --
        //
        // The ONLY way to set asyncio.current_task(). CPython's own
        // Task.__step calls them. No public alternative exists.
        //
        // Python-level API: asyncio.tasks._enter_task(loop, task)
        // C-level: _PyTask_Enter / _PyTask_Leave (not exposed to Python)
        //
        // Version history:
        //   3.7-3.13: updates a global dict (state->current_tasks)
        //   3.14+: writes to PyThreadState.asyncio_current_task
        //   Python-level signature unchanged across all versions.
        //
        // If removed: check for loop._current_task (proposed public alt).
        // Tracking: https://github.com/python/cpython/issues/120974
        //           https://discuss.python.org/t/store-current-task-on-the-loop/75926
        let tasks_mod = py
            .import(c"asyncio.tasks")
            .map_err(|e| format!("import asyncio.tasks: {e}"))?;
        let enter_task = tasks_mod
            .getattr(c"_enter_task")
            .map_err(|e| {
                format!(
                    "missing asyncio.tasks._enter_task — asyncio internals changed? \
                     See https://github.com/python/cpython/issues/120974: {e}"
                )
            })?
            .unbind();
        let leave_task = tasks_mod
            .getattr(c"_leave_task")
            .map_err(|e| {
                format!(
                    "missing asyncio.tasks._leave_task — asyncio internals changed? \
                     See https://github.com/python/cpython/issues/120974: {e}"
                )
            })?
            .unbind();
        let task_mod = py
            .import(c"apx._task")
            .map_err(|e| format!("import apx._task: {e}"))?;
        let scheduler_task_cls = task_mod
            .getattr(c"_SchedulerTask")
            .map_err(|e| format!("missing _SchedulerTask: {e}"))?
            .unbind();

        // Cache call_soon_threadsafe (thread-safe variant, needed since
        // the asyncio loop now runs on a dedicated thread).
        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .map_err(|e| format!("missing call_soon_threadsafe: {e}"))?
            .unbind();
        let call_soon = event_loop
            .getattr(c"call_soon")
            .map_err(|e| format!("missing call_soon: {e}"))?
            .unbind();
        let task_ops = TaskOps {
            enter_task,
            leave_task,
            scheduler_task_cls,
            call_soon,
            loop_obj: event_loop.clone().unbind(),
        };

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

        tracing::info!("reactor initialized (dedicated asyncio thread)");

        Ok(Self {
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            task_ops,
            asyncio_thread: Mutex::new(Some(asyncio_thread)),
        })
    }

    /// Get the cached `call_soon_threadsafe` method.
    pub fn call_soon_threadsafe(&self) -> &Py<PyAny> {
        &self.call_soon_threadsafe
    }

    /// Get the cached task lifecycle operations.
    pub fn task_ops(&self) -> &TaskOps {
        &self.task_ops
    }

    /// Get a reference to the Python asyncio event loop object.
    pub fn event_loop_ref(&self) -> &Py<PyAny> {
        &self.event_loop
    }

    /// Shut down the reactor.
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

        // 3. Clean up: async generators → executor → pending tasks → close.
        Python::attach(|py| {
            let el = self.event_loop.bind(py);
            if let Ok(events) = py.import(c"asyncio.events") {
                let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            }
            shutdown_asyncgens(py, el);
            shutdown_default_executor(py, el);
            cancel_pending_tasks(py, el);
            let _ = el.call_method0(c"close");
        });
    }
}

impl std::fmt::Debug for Reactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reactor").finish_non_exhaustive()
    }
}
