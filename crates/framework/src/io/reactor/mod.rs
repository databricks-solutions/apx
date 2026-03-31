//! Asyncio event loop lifecycle — init, shutdown, task submission.
//!
//! The [`Reactor`] manages the Python asyncio event loop on a dedicated
//! thread. It owns the loop object, `call_soon_threadsafe`, and
//! `create_task` for coroutine submission.

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
                    tracing::error!(name: "apx.reactor.asyncio_import_failed", "failed to import asyncio for uvloop policy install");
                    return;
                };
                let Ok(policy_obj) = uvloop.call_method0(c"EventLoopPolicy") else {
                    tracing::error!(name: "apx.reactor.uvloop_event_loop_policy_failed", "uvloop.EventLoopPolicy() call failed");
                    return;
                };
                if let Err(e) = asyncio.call_method1(c"set_event_loop_policy", (policy_obj,)) {
                    tracing::error!(name: "apx.reactor.set_event_loop_policy_failed", error = %e, "asyncio.set_event_loop_policy() failed");
                    return;
                }
                tracing::debug!(name: "apx.reactor.uvloop_policy_installed", "installed uvloop event loop policy");
            }
            Err(e) => {
                tracing::warn!(name: "apx.reactor.uvloop_unavailable_fallback", error = %e, "uvloop not available, falling back to asyncio");
            }
        }
    } else {
        tracing::debug!(name: "apx.reactor.event_loop_policy", policy, "using asyncio event loop policy");
    }
}

/// Create an asyncio event loop.
fn create_event_loop(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    tracing::debug!(name: "apx.reactor.creating_event_loop", "creating asyncio event loop");
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
fn shutdown_asyncgens(_py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    let Ok(coro) = event_loop.call_method0(c"shutdown_asyncgens") else {
        return;
    };
    if let Err(e) = event_loop.call_method1(c"run_until_complete", (&coro,)) {
        tracing::warn!(name: "apx.reactor.shutdown_asyncgens_failed", error = %e, "shutdown_asyncgens failed");
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
        tracing::warn!(name: "apx.reactor.shutdown_default_executor_failed", error = %e, "shutdown_default_executor failed");
    }
}

// ── Reactor ──────────────────────────────────────────────────────────────

/// Asyncio event loop lifecycle manager.
///
/// Owns the Python asyncio event loop running on a dedicated OS thread,
/// the cached `call_soon_threadsafe` and `create_task` bound methods.
pub struct Reactor {
    /// Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Cached `loop.call_soon_threadsafe` bound method.
    call_soon_threadsafe: Py<PyAny>,
    /// Cached `loop.create_task` bound method.
    create_task: Py<PyAny>,
    /// Dedicated OS thread running `loop.run_forever()`.
    asyncio_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Reactor {
    /// Initialize the reactor on the current thread.
    ///
    /// Sets up the asyncio event loop, marks it as running, enables eager
    /// task factory (Python 3.12+), caches submission callables, and
    /// spawns a dedicated OS thread running `run_forever()`.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization fails.
    pub fn init(py: Python<'_>, loop_policy: &str) -> Result<Self, String> {
        install_loop_policy(py, loop_policy);

        let event_loop = create_event_loop(py).map_err(|e| format!("create_event_loop: {e}"))?;

        let asyncio = py
            .import(c"asyncio")
            .map_err(|e| format!("import asyncio: {e}"))?;
        asyncio
            .call_method1(c"set_event_loop", (&event_loop,))
            .map_err(|e| format!("set_event_loop: {e}"))?;

        // Mark as running loop so asyncio.get_running_loop() works for
        // libraries (Starlette middleware, DB drivers, etc.).
        let events = py
            .import(c"asyncio.events")
            .map_err(|e| format!("import asyncio.events: {e}"))?;
        events
            .call_method1(c"_set_running_loop", (&event_loop,))
            .map_err(|e| format!("_set_running_loop: {e}"))?;
        tracing::debug!(name: "apx.reactor.set_running_loop_installed", "reactor: _set_running_loop installed");

        // Eager task factory (Python 3.12+).
        if let Ok(eager_factory) = asyncio.getattr(c"eager_task_factory") {
            match event_loop.call_method1(c"set_task_factory", (eager_factory,)) {
                Ok(_) => {
                    tracing::debug!(name: "apx.reactor.eager_task_factory_enabled", "eager task factory enabled (Python 3.12+)");
                }
                Err(e) => {
                    tracing::debug!(name: "apx.reactor.eager_task_factory_unavailable", "eager task factory not available: {e}");
                }
            }
        }

        let call_soon_threadsafe = event_loop
            .getattr(c"call_soon_threadsafe")
            .map_err(|e| format!("missing call_soon_threadsafe: {e}"))?
            .unbind();
        let create_task = event_loop
            .getattr(c"create_task")
            .map_err(|e| format!("missing create_task: {e}"))?
            .unbind();

        // Spawn dedicated asyncio thread with tokio handle for I/O.
        let el_for_thread = event_loop.clone().unbind();
        let tokio_handle = tokio::runtime::Handle::try_current().ok();
        let asyncio_thread = std::thread::Builder::new()
            .name("apx-asyncio".to_owned())
            .spawn(move || {
                if let Some(handle) = tokio_handle {
                    super::set_tokio_handle(handle);
                }
                Python::attach(|py| {
                    let el = el_for_thread.bind(py);
                    if let Err(e) = el.call_method0(c"run_forever") {
                        tracing::error!(name: "apx.reactor.run_forever_failed", error = %e, "asyncio thread: run_forever failed");
                    }
                });
            })
            .map_err(|e| format!("spawn asyncio thread: {e}"))?;

        tracing::debug!(name: "apx.reactor.initialized", "reactor initialized (asyncio delegation)");

        Ok(Self {
            event_loop: event_loop.unbind(),
            call_soon_threadsafe,
            create_task,
            asyncio_thread: Mutex::new(Some(asyncio_thread)),
        })
    }

    /// The Python asyncio event loop object.
    pub fn event_loop_py(&self) -> &Py<PyAny> {
        &self.event_loop
    }

    /// Cached `loop.call_soon_threadsafe` bound method.
    pub fn call_soon_threadsafe(&self) -> &Py<PyAny> {
        &self.call_soon_threadsafe
    }

    /// Cached `loop.create_task` bound method.
    pub fn create_task(&self) -> &Py<PyAny> {
        &self.create_task
    }

    /// Shut down the reactor.
    ///
    /// 1. Stops the asyncio loop (wakes `run_forever` via `call_soon_threadsafe`).
    /// 2. Joins the dedicated asyncio thread.
    /// 3. Cancels pending tasks and closes the loop.
    pub fn shutdown(&self) {
        Python::attach(|py| {
            let el = self.event_loop.bind(py);
            if let Ok(stop) = el.getattr(c"stop") {
                let _ = el.call_method1(c"call_soon_threadsafe", (stop,));
            }
        });

        let handle = self
            .asyncio_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(h) = handle
            && let Err(e) = h.join()
        {
            tracing::warn!(name: "apx.reactor.asyncio_thread_panicked", "asyncio thread panicked: {e:?}");
        }

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

crate::opaque_debug!(Reactor);
