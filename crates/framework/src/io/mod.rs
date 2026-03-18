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
    poke_ops: PokeOps,
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

        // 6. Cache noop callable and `_ready` deque for conditional poke.
        let cached_noop = py
            .eval(c"lambda: None", None, None)
            .map_err(|e| format!("cache noop: {e}"))?
            .unbind();
        let ready_deque: Option<Py<PyAny>> = reactor.event_loop_ref().getattr(py, c"_ready").ok();
        let poke_notify = Arc::new(tokio::sync::Notify::new());
        tracing::info!(
            has_ready_deque = ready_deque.is_some(),
            "poke: cached noop + _ready introspection"
        );

        let poke_ops = PokeOps {
            cached_noop,
            ready_deque,
            poke_notify: Arc::clone(&poke_notify),
        };

        // 7. Create notify for drain task wake.
        let drain_notify = Arc::new(tokio::sync::Notify::new());

        // 8. Set notify-based wake on the ready queue.
        ready_queue.set_notify_wake(Arc::clone(&drain_notify));

        // 9. Create DrainOnLoop callback — drives ready tasks on the asyncio thread.
        let tokio_handle = tokio::runtime::Handle::try_current()
            .map_err(|e| format!("DrainOnLoop needs tokio context: {e}"))?;
        let drain_on_loop = Py::new(
            py,
            DrainOnLoop {
                queue: Arc::clone(&ready_queue),
                ops: Arc::clone(&coroutine_ops),
                call_soon_threadsafe: reactor.call_soon_threadsafe().clone_ref(py),
                task_ops: reactor.task_ops().clone_ref(py),
                tokio_handle,
            },
        )
        .map_err(|e| format!("DrainOnLoop::new: {e}"))?;

        // 10. Spawn the drain task — wakes on Notify, schedules DrainOnLoop
        //     on the asyncio thread via call_soon_threadsafe.
        let drain_cs = reactor.call_soon_threadsafe().clone_ref(py);
        let drain_cb = drain_on_loop.clone_ref(py);
        let notify = Arc::clone(&drain_notify);
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                tracing::trace!("drain_task: woke up, scheduling on asyncio thread");
                Python::attach(|py| {
                    if let Err(e) = drain_cs.call1(py, (&drain_cb,)) {
                        tracing::debug!(error = %e, "drain_task: call_soon_threadsafe failed");
                    }
                });
            }
        });

        // 10. Spawn the coalesced poke task (uvloop fallback path).
        let poke_cs = reactor.call_soon_threadsafe().clone_ref(py);
        let poke_noop = poke_ops.cached_noop.clone_ref(py);
        let poke_listen = Arc::clone(&poke_notify);
        tokio::spawn(async move {
            loop {
                poke_listen.notified().await;
                Python::attach(|py| {
                    if let Err(e) = poke_cs.call1(py, (poke_noop.clone_ref(py),)) {
                        tracing::debug!(error = %e, "poke_task: call_soon_threadsafe failed");
                    }
                });
            }
        });

        tracing::info!("event loop initialized (composition root)");

        Ok(Self {
            reactor,
            coroutine_ops,
            ready_queue,
            drain_notify,
            poke_ops,
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

    /// Cached state for conditional event loop poke.
    pub fn poke_ops(&self) -> &PokeOps {
        &self.poke_ops
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

// ── DrainOnLoop — drives ready tasks on the asyncio thread ───────────────

/// Callback scheduled via `call_soon_threadsafe` that drains the
/// [`ReadyQueue`] on the asyncio thread.
///
/// Running on the asyncio thread makes per-step `_enter_task`/`_leave_task`
/// safe: `_run_once` processes callbacks sequentially, so there is no
/// concurrent `_enter_task` from the reactor's own task stepping.
#[pyclass(module = "apx._core")]
struct DrainOnLoop {
    queue: Arc<ReadyQueue>,
    ops: Arc<dyn CoroutineOps>,
    call_soon_threadsafe: Py<PyAny>,
    task_ops: TaskOps,
    tokio_handle: tokio::runtime::Handle,
}

#[pymethods]
impl DrainOnLoop {
    fn __call__(&self, py: Python<'_>) -> PyResult<()> {
        set_tokio_handle(self.tokio_handle.clone());
        let mut count: usize = 0;
        while let Some(ready) = self.queue.pop() {
            count += 1;
            if let Err(e) = bridge::resume_task(
                py,
                ready,
                &self.ops,
                &self.call_soon_threadsafe,
                &self.queue,
                &self.task_ops,
                true,
            ) {
                tracing::warn!(error = %e, "drain_on_loop: resume failed");
            }
        }
        if count > 0 {
            tracing::trace!(count, "drain_on_loop: drained on asyncio thread");
        }
        Ok(())
    }
}

// ── Conditional poke ─────────────────────────────────────────────────────

/// Cached state for conditionally waking the asyncio event loop.
///
/// On CPython, `ready_deque` holds a reference to `loop._ready` for
/// delta-based poke decisions. On uvloop (where `_ready` doesn't exist),
/// `poke_notify` signals a dedicated coalesced poke task instead.
pub struct PokeOps {
    /// `lambda: None` evaluated once at init, reused for every poke.
    pub cached_noop: Py<PyAny>,
    /// `loop._ready` deque (CPython only, `None` on uvloop).
    pub ready_deque: Option<Py<PyAny>>,
    /// Notify handle for the coalesced poke task (uvloop path).
    pub poke_notify: Arc<tokio::sync::Notify>,
}

impl PokeOps {
    /// Read `len(loop._ready)` when available (CPython); 0 on uvloop.
    ///
    /// Uses `PyObject_Length` FFI directly via `Bound::len()` for ~1-2us
    /// savings over `__len__` method dispatch.
    pub fn ready_len(&self, py: Python<'_>) -> usize {
        self.ready_deque
            .as_ref()
            .and_then(|d| d.bind(py).len().ok())
            .unwrap_or(0)
    }

    /// Poke only when `_ready` grew during the drive cycle.
    ///
    /// `n_before` must be captured **after** `create_scheduler_task` so the
    /// sentinel `__step` (3.11) is already reflected.  Any growth beyond
    /// that means user code called `loop.create_task()` or similar.
    ///
    /// On CPython: compares `_ready` length before/after the drive.
    /// On uvloop: signals the coalesced poke task via `Notify`.
    pub fn maybe_poke(
        &self,
        py: Python<'_>,
        n_before: usize,
        n_after: usize,
        call_soon_threadsafe: &Py<PyAny>,
    ) {
        match &self.ready_deque {
            Some(_) => {
                if n_after > n_before {
                    tracing::trace!(
                        delta = n_after.saturating_sub(n_before),
                        "poke: _ready grew during drive, poking synchronously"
                    );
                    let _ = call_soon_threadsafe.call1(py, (&self.cached_noop,));
                }
            }
            None => {
                self.poke_notify.notify_one();
            }
        }
    }
}

impl std::fmt::Debug for PokeOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PokeOps")
            .field("has_ready_deque", &self.ready_deque.is_some())
            .finish_non_exhaustive()
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

/// Run a closure with a tokio runtime handle.
///
/// Checks the thread-local first (set via [`set_tokio_handle`]), then
/// falls back to [`tokio::runtime::Handle::try_current`] which succeeds
/// when a `Handle::enter` guard is active (e.g. on the asyncio thread
/// during [`DrainOnLoop::__call__`]).
pub fn with_tokio_handle<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&tokio::runtime::Handle) -> R,
{
    TOKIO_HANDLE.with(|cell| {
        if let Some(h) = cell.borrow().as_ref() {
            return Some(f(h));
        }
        tokio::runtime::Handle::try_current().ok().map(|h| f(&h))
    })
}
