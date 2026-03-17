//! Python I/O interop — Rust↔asyncio boundary.
//!
//! Three bounded contexts:
//! - [`driver`] — coroutine stepping engine (PyIter_Send, classify, coro stack)
//! - [`reactor`] — asyncio event loop lifecycle (init, shutdown, task registration)
//! - [`bridge`] — driver↔reactor coordination (ReadyQueue, callbacks, spawn)
//!
//! [`EventLoop`] is the composition root — it creates and wires all three.
//!
//! # Private API dependencies
//!
//! | API | Location | Type | Fallback | Tracking |
//! |---|---|---|---|---|
//! | `asyncio.tasks._enter_task` | reactor | Essential | None (no public alt) | [CPython #120974](https://github.com/python/cpython/issues/120974) |
//! | `asyncio.tasks._leave_task` | reactor | Essential | None | [CPython #120974](https://github.com/python/cpython/issues/120974) |
//! | `_PyDict_NewPresized` | driver/ffi | Optional | `PyDict::new()` | Stable 3.8-3.13 |
//! | `_asyncio_future_blocking` | driver/ffi | De facto stable | `isinstance` check | Stable since 3.4 |

pub mod bridge;
pub mod counters;
pub mod driver;
pub mod reactor;

use std::sync::Arc;

use pyo3::prelude::*;

use bridge::queue::ReadyQueue;
use counters::SchedulerCounters;
use driver::ffi::{CoroutineOps, FfiCoroutineOps};
use reactor::TaskOps;

// ── EventLoop ────────────────────────────────────────────────────────────

/// Composition root — creates and wires the driver, reactor, and bridge.
///
/// Owns a [`reactor::Reactor`] (asyncio lifecycle), an [`Arc<dyn CoroutineOps>`]
/// (coroutine stepping), and an [`Arc<ReadyQueue>`] (driver↔reactor bridge).
/// Accessors delegate to the reactor for `call_soon_threadsafe` and `task_ops`.
pub struct EventLoop {
    reactor: reactor::Reactor,
    coroutine_ops: Arc<dyn CoroutineOps>,
    ready_queue: Arc<ReadyQueue>,
    #[expect(dead_code, reason = "Arc kept alive for spawned drain task")]
    drain_notify: Arc<tokio::sync::Notify>,
}

impl EventLoop {
    /// Initialize the event loop on the current thread.
    ///
    /// 1. Creates the asyncio reactor (event loop, thread, task ops).
    /// 2. Resolves FFI coroutine ops for the driver.
    /// 3. Creates the ready queue (driver↔reactor bridge).
    /// 4. Initializes scheduler counters.
    /// 5. Stores the tokio runtime handle in the thread-local.
    /// 6. Spawns the drain task that re-drives ready tasks on notify.
    ///
    /// # Errors
    ///
    /// Returns an error if Python initialization fails.
    pub fn init(py: Python<'_>, loop_policy: &str) -> Result<Self, String> {
        // 1. Create the asyncio reactor.
        let reactor = reactor::Reactor::init(py, loop_policy)?;

        // 2. Resolve FFI coroutine ops.
        let coroutine_ops: Arc<dyn CoroutineOps> = Arc::new(
            FfiCoroutineOps::resolve(py).map_err(|e| format!("FfiCoroutineOps::resolve: {e}"))?,
        );

        // 3. Create the ready queue.
        let ready_queue = Arc::new(ReadyQueue::new());

        // 4. Initialize scheduler counters.
        let scheduler_counters = Arc::new(SchedulerCounters::new());
        counters::init(Arc::clone(&scheduler_counters));

        // 5. Store the tokio runtime handle in the thread-local.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            set_tokio_handle(handle);
        }
        tracing::info!("rust scheduler initialized (no asyncio monkeypatching)");

        // 6. Create notify for drain task wake.
        let drain_notify = Arc::new(tokio::sync::Notify::new());

        // 7. Set notify-based wake on the ready queue.
        ready_queue.set_notify_wake(Arc::clone(&drain_notify));

        // 8. Spawn the drain task on the current-thread tokio runtime.
        let rq = Arc::clone(&ready_queue);
        let ct = Arc::clone(&coroutine_ops);
        let cs = reactor.call_soon_threadsafe().clone_ref(py);
        let notify = Arc::clone(&drain_notify);
        let drain_enter = reactor.task_ops().enter_task.clone_ref(py);
        let drain_leave = reactor.task_ops().leave_task.clone_ref(py);
        let drain_cls = reactor.task_ops().scheduler_task_cls.clone_ref(py);
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                tracing::trace!("drain_task: woke up");
                Python::attach(|py| {
                    let drain_ops = TaskOps {
                        enter_task: drain_enter.clone_ref(py),
                        leave_task: drain_leave.clone_ref(py),
                        scheduler_task_cls: drain_cls.clone_ref(py),
                    };
                    let count = rq.drain(py, &ct, &cs, &rq, &drain_ops);
                    if count > 0 {
                        // Wake the asyncio loop — drain may have created
                        // _SchedulerTasks or asyncio tasks that added items
                        // to `_ready` via `call_soon`. See `poke_event_loop`.
                        let noop = py.eval(c"lambda: None", None, None);
                        if let Ok(noop) = noop {
                            let _ = cs.call1(py, (noop,));
                        }
                    }
                    tracing::trace!(count, "drain_task: drained");
                });
            }
        });

        tracing::info!("event loop initialized (composition root)");

        Ok(Self {
            reactor,
            coroutine_ops,
            ready_queue,
            drain_notify,
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
        self.reactor.call_soon_threadsafe()
    }

    /// Get the cached task lifecycle operations.
    pub fn task_ops(&self) -> &TaskOps {
        self.reactor.task_ops()
    }

    /// Shut down the event loop.
    ///
    /// Delegates to [`reactor::Reactor::shutdown`] which stops the asyncio loop,
    /// joins the dedicated thread, cancels pending tasks, and closes the loop.
    pub fn shutdown(&self) {
        self.reactor.shutdown();
    }
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineEventLoop").finish_non_exhaustive()
    }
}

// ── Thread-local tokio runtime handle ────────────────────────────────────

use std::cell::RefCell;

thread_local! {
    /// Tokio runtime handle cached on the event loop thread.
    ///
    /// Set once during [`EventLoop::init`] when the Rust scheduler is installed.
    static TOKIO_HANDLE: RefCell<Option<tokio::runtime::Handle>> = const { RefCell::new(None) };
}

/// Store the tokio runtime handle for the current (event loop) thread.
pub fn set_tokio_handle(handle: tokio::runtime::Handle) {
    TOKIO_HANDLE.with(|cell| *cell.borrow_mut() = Some(handle));
}

/// Run a closure with the thread-local tokio handle, if available.
pub fn with_tokio_handle<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&tokio::runtime::Handle) -> R,
{
    TOKIO_HANDLE.with(|cell| cell.borrow().as_ref().map(f))
}
