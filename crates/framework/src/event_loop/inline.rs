//! Single-thread event loop for worker processes.
//!
//! Initializes the asyncio event loop as dormant (installed but not running
//! `run_forever()`). The Rust scheduler drives coroutines inline on the
//! tokio thread — no separate OS thread, no QueueDrainer, no pipe wake.

use std::sync::Arc;

use pyo3::prelude::*;

use super::core::{
    cancel_pending_tasks, create_event_loop, install_loop_policy, install_rust_scheduler,
};
use crate::scheduler::driver::CachedTypes;
use crate::scheduler::queue::ReadyQueue;

/// Single-thread event loop for worker processes.
///
/// Initializes the asyncio event loop as dormant (installed but not running
/// `run_forever()`). The Rust scheduler drives coroutines inline on the
/// tokio thread.
pub struct InlineEventLoop {
    /// Python asyncio event loop object.
    event_loop: Py<PyAny>,
    /// Pre-resolved Python type references.
    cached_types: Arc<CachedTypes>,
    /// Per-worker ready queue for suspended tasks.
    ready_queue: Arc<ReadyQueue>,
    /// Cached `loop.call_soon` bound method (local — safe, same thread).
    call_soon: Py<PyAny>,
    /// Notify for waking the drain task when ready queue has items.
    /// Held to keep the Arc alive for the spawned drain task.
    #[expect(dead_code, reason = "Arc kept alive for spawned drain task")]
    drain_notify: Arc<tokio::sync::Notify>,
    /// Notify for waking the asyncio loop pump task.
    /// Held to keep the Arc alive for the spawned pump task.
    #[expect(dead_code, reason = "Arc kept alive for spawned pump task")]
    pump_notify: Arc<tokio::sync::Notify>,
}

impl InlineEventLoop {
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

        // 6. Resolve cached types.
        let cached_types =
            Arc::new(CachedTypes::resolve(py).map_err(|e| format!("CachedTypes::resolve: {e}"))?);

        // 7. Create ready queue.
        let ready_queue = Arc::new(ReadyQueue::new());

        // 8. Cache call_soon.
        let call_soon = event_loop
            .getattr(c"call_soon")
            .map_err(|e| format!("missing call_soon: {e}"))?
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
        let ct = Arc::clone(&cached_types);
        let cs = call_soon.clone_ref(py);
        let notify = Arc::clone(&drain_notify);
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                Python::attach(|py| {
                    rq.drain(py, &ct, &cs, &rq);
                });
            }
        });

        // 13. Create pump notify and wire to ready queue.
        let pump_notify = Arc::new(tokio::sync::Notify::new());
        ready_queue.set_pump_notify(Arc::clone(&pump_notify));

        // 14. Probe for _run_once (private but stable on CPython + uvloop).
        let has_run_once = event_loop.hasattr(c"_run_once").unwrap_or(false);

        // 15. Spawn the asyncio loop pump task.
        let pump_n = Arc::clone(&pump_notify);
        let pump_rq = Arc::clone(&ready_queue);
        let pump_el = event_loop.clone().unbind();
        tokio::spawn(async move {
            loop {
                pump_n.notified().await;
                while pump_rq.pending_asyncio_count() > 0 {
                    Python::attach(|py| {
                        let el = pump_el.bind(py);
                        if has_run_once {
                            let _ = el.call_method0(c"_run_once");
                        } else if let Ok(asyncio) = py.import(c"asyncio")
                            && let Ok(coro) = asyncio.call_method1(c"sleep", (0,))
                        {
                            let _ = el.call_method1(c"run_until_complete", (coro,));
                        }
                    });
                    tokio::task::yield_now().await;
                }
            }
        });

        tracing::info!("inline event loop initialized (no dedicated asyncio thread)");

        Ok(Self {
            event_loop: event_loop.unbind(),
            cached_types,
            ready_queue,
            call_soon,
            drain_notify,
            pump_notify,
        })
    }

    /// Get the Python event loop object.
    pub fn event_loop_ref(&self) -> &Py<PyAny> {
        &self.event_loop
    }

    /// Get the cached Python types.
    pub fn cached_types(&self) -> &Arc<CachedTypes> {
        &self.cached_types
    }

    /// Get the ready queue.
    pub fn ready_queue(&self) -> &Arc<ReadyQueue> {
        &self.ready_queue
    }

    /// Get the cached `call_soon` method.
    pub fn call_soon(&self) -> &Py<PyAny> {
        &self.call_soon
    }

    /// Shut down the inline event loop.
    ///
    /// Cancels pending tasks and closes the loop. Must be called with the
    /// GIL held on the same thread that initialized the loop.
    pub fn shutdown(&self) {
        Python::attach(|py| {
            let event_loop = self.event_loop.bind(py);

            // Clear the running loop marker.
            if let Ok(events) = py.import(c"asyncio.events") {
                let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            }

            cancel_pending_tasks(py, event_loop);
            let _ = event_loop.call_method0(c"close");
        });
    }
}

impl std::fmt::Debug for InlineEventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineEventLoop").finish_non_exhaustive()
    }
}
