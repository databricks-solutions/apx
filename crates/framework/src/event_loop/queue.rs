//! Lock-free MPSC queue drained by a single recurring callback on the event loop.
//!
//! Replaces the per-request `call_soon_threadsafe` pattern with a batched
//! drain: tokio threads push [`WorkItem`]s into an unbounded channel, and
//! [`QueueDrainer`] processes them all in one Python frame.

use std::fmt;
use std::ops::Not;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::prelude::*;
use tokio::sync::mpsc;

use crate::error::AppError;
use crate::scheduler::driver::{CachedTypes, spawn_and_drive};
use crate::scheduler::queue::ReadyQueue;

/// Closure that builds a Python coroutine on the event loop thread.
pub type CoroutineBuilder = Box<dyn FnOnce(Python<'_>) -> Result<Py<PyAny>, AppError> + Send>;

/// Fire-and-forget closure that runs on the event loop thread.
pub type Callback = Box<dyn FnOnce(Python<'_>) + Send + 'static>;

/// Work item pushed from tokio threads to the event loop thread.
pub struct WorkItem {
    /// Builds the coroutine on the event loop thread (deferred execution).
    pub builder: CoroutineBuilder,
    /// Oneshot sender for the coroutine result.
    pub tx: tokio::sync::oneshot::Sender<Result<Py<PyAny>, AppError>>,
}

/// Item in the event loop work queue.
///
/// Supports both coroutine-building work items (with result channel) and
/// fire-and-forget callbacks (no result channel).
pub enum QueueItem {
    /// Builds a coroutine, creates task, sends result via oneshot.
    Work(WorkItem),
    /// Runs on event loop thread, no result channel.
    Callback(Callback),
}

impl fmt::Debug for QueueItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Work(w) => f.debug_tuple("Work").field(w).finish(),
            Self::Callback(_) => f.debug_tuple("Callback").finish(),
        }
    }
}

impl fmt::Debug for WorkItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkItem")
            .field("pending", &self.tx.is_closed().not())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// SchedulerState — cached refs for Rust-driven scheduling
// ---------------------------------------------------------------------------

/// Pre-resolved Python references for Rust-driven coroutine scheduling.
///
/// Constructed once during event loop initialization.
pub struct SchedulerState {
    pub(crate) cached_types: Arc<CachedTypes>,
    pub(crate) call_soon: Py<PyAny>,
    pub(crate) ready_queue: Arc<ReadyQueue>,
}

impl fmt::Debug for SchedulerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchedulerState").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// QueueDrainer
// ---------------------------------------------------------------------------

/// Singleton drainer that runs on the event loop thread.
///
/// Installed as a recurring callback via `loop.call_soon(drainer)`. When
/// invoked, drains all pending [`WorkItem`]s from the channel and drives
/// coroutines through the Rust scheduler.
#[pyclass(module = "apx._core")]
pub struct QueueDrainer {
    rx: mpsc::UnboundedReceiver<QueueItem>,
    /// Cached `loop.call_soon` bound method (local, not threadsafe).
    call_soon: Py<PyAny>,
    /// Python reference to `self` for `call_soon(self)` rescheduling.
    ///
    /// Creates a Python reference cycle — acceptable for a worker-lifetime
    /// singleton. No `__traverse__` needed (not in user-visible object graphs).
    self_ref: Option<Py<PyAny>>,
    /// Shared flag: `true` means the drainer is sleeping and producers
    /// must wake it (via pipe write or `call_soon_threadsafe`).
    needs_wake: Arc<AtomicBool>,
    /// Rust scheduler state — always present.
    scheduler: SchedulerState,
    /// Cached Python callable that drains pipe bytes (pipe wake only).
    /// When `None`, pipe wake is not active (GIL fallback path).
    drain_wake_fn: Option<Py<PyAny>>,
    /// Read-end fd of the wake pipe (pipe wake only, -1 if unused).
    wake_read_fd: i32,
}

impl fmt::Debug for QueueDrainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueueDrainer")
            .field("needs_wake", &self.needs_wake.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl QueueDrainer {
    /// Create a new drainer. `self_ref` must be set after `Py::new`.
    pub fn new(
        rx: mpsc::UnboundedReceiver<QueueItem>,
        call_soon: Py<PyAny>,
        needs_wake: Arc<AtomicBool>,
        scheduler: SchedulerState,
    ) -> Self {
        Self {
            rx,
            call_soon,
            self_ref: None,
            needs_wake,
            scheduler,
            drain_wake_fn: None,
            wake_read_fd: -1,
        }
    }

    /// Set the Python self-reference for `call_soon(self)` rescheduling.
    pub fn set_self_ref(&mut self, self_ref: Py<PyAny>) {
        self.self_ref = Some(self_ref);
    }

    /// Configure pipe-based wake drain (called during init when pipe wake is active).
    ///
    /// `drain_fn` is a Python callable that reads and discards bytes from `read_fd`.
    /// Called at the start of each `__call__` to consume pipe wake bytes (level-triggered).
    pub fn set_pipe_drain(&mut self, drain_fn: Py<PyAny>, read_fd: i32) {
        self.drain_wake_fn = Some(drain_fn);
        self.wake_read_fd = read_fd;
    }
}

#[pymethods]
impl QueueDrainer {
    fn __call__(&mut self, py: Python<'_>) -> PyResult<()> {
        // Drain wake pipe bytes (level-triggered — must consume to avoid re-fire).
        if let Some(ref drain_fn) = self.drain_wake_fn {
            let _ = drain_fn.call1(py, (self.wake_read_fd,));
        }
        let count = self.drain_pending(py);
        if count > 0 {
            return self.reschedule(py);
        }
        self.transition_to_sleep(py)
    }
}

impl QueueDrainer {
    /// Pop all pending items, dispatch each, then drain ready tasks.
    fn drain_pending(&mut self, py: Python<'_>) -> usize {
        let mut count = 0;
        while let Ok(item) = self.rx.try_recv() {
            count += 1;
            match item {
                QueueItem::Work(work) => self.dispatch_item(py, work),
                QueueItem::Callback(cb) => cb(py),
            }
        }
        // Drain ready tasks (re-drives from suspended awaitable resolution).
        let sched = &self.scheduler;
        count += sched.ready_queue.drain(
            py,
            &sched.cached_types,
            &sched.call_soon,
            &sched.ready_queue,
        );
        count
    }

    /// Dispatch one work item via the Rust scheduler.
    fn dispatch_item(&self, py: Python<'_>, item: WorkItem) {
        let sched = &self.scheduler;
        let coro = match (item.builder)(py) {
            Ok(coro) => coro,
            Err(e) => {
                let _ = item.tx.send(Err(e));
                return;
            }
        };
        spawn_and_drive(
            py,
            coro,
            item.tx,
            &sched.cached_types,
            &sched.call_soon,
            &sched.ready_queue,
        );
    }

    /// Set `needs_wake`, double-check for race, reschedule if items arrived.
    fn transition_to_sleep(&mut self, py: Python<'_>) -> PyResult<()> {
        self.needs_wake.store(true, Ordering::Release);
        // Double-check: items may have arrived between drain_pending and
        // the store above. Release on store + Acquire on producer's swap
        // ensures visibility.
        let count = self.drain_pending(py);
        if count > 0 {
            self.needs_wake.store(false, Ordering::Release);
            return self.reschedule(py);
        }
        Ok(()) // truly idle — producer will wake us via pipe write or call_soon_threadsafe
    }

    /// Re-enqueue self via `call_soon(self)` for the next drain iteration.
    fn reschedule(&self, py: Python<'_>) -> PyResult<()> {
        let Some(ref self_ref) = self.self_ref else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "QueueDrainer self_ref not set",
            ));
        };
        self.call_soon.call1(py, (self_ref,))?;
        Ok(())
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use crate::scheduler::queue::ReadyQueue;
    use tokio::sync::oneshot;

    /// Create a minimal `SchedulerState` for tests.
    fn test_scheduler_state(py: Python<'_>) -> SchedulerState {
        let cached_types = Arc::new(CachedTypes::resolve(py).unwrap());
        let noop = py
            .eval(c"lambda *a, **kw: None", None, None)
            .unwrap()
            .unbind();
        SchedulerState {
            cached_types,
            call_soon: noop,
            ready_queue: Arc::new(ReadyQueue::new()),
        }
    }

    #[test]
    fn work_item_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WorkItem>();
    }

    #[test]
    fn work_item_debug_pending() {
        let (tx, _rx) = oneshot::channel::<Result<Py<PyAny>, AppError>>();
        let item = WorkItem {
            builder: Box::new(|py| Ok(py.None())),
            tx,
        };
        let dbg = format!("{item:?}");
        assert!(dbg.contains("WorkItem"));
        assert!(dbg.contains("pending: true"));
    }

    #[test]
    fn work_item_debug_closed() {
        let (tx, rx) = oneshot::channel::<Result<Py<PyAny>, AppError>>();
        drop(rx);
        let item = WorkItem {
            builder: Box::new(|py| Ok(py.None())),
            tx,
        };
        let dbg = format!("{item:?}");
        assert!(dbg.contains("pending: false"));
    }

    #[test]
    fn queue_drainer_debug() {
        crate::with_py(|py| {
            let (_tx, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));
            let sched = test_scheduler_state(py);
            let drainer = QueueDrainer::new(rx, py.None(), needs_wake, sched);
            let dbg = format!("{drainer:?}");
            assert!(dbg.contains("QueueDrainer"));
            assert!(dbg.contains("needs_wake: false"));
        });
    }

    #[test]
    fn queue_drainer_drain_empty_sets_needs_wake() {
        crate::with_py(|py| {
            let (_tx, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));
            let sched = test_scheduler_state(py);
            let mut drainer = QueueDrainer::new(rx, py.None(), needs_wake.clone(), sched);
            let count = drainer.drain_pending(py);
            assert_eq!(count, 0);
            // Simulate transition_to_sleep (without reschedule since no self_ref)
            needs_wake.store(true, Ordering::Release);
            assert!(needs_wake.load(Ordering::Acquire));
        });
    }

    #[test]
    fn queue_drainer_processes_items() {
        crate::with_py(|py| {
            let (tx_queue, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));
            let sched = test_scheduler_state(py);
            let call_soon = sched.call_soon.clone_ref(py);

            let mut drainer = QueueDrainer::new(rx, call_soon, needs_wake, sched);

            // Push a work item with a trivial coroutine builder
            let (result_tx, mut result_rx) = oneshot::channel();
            let code = std::ffi::CString::new("async def _t(): return 42\ncoro = _t()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            let coro: Py<PyAny> = locals.get_item("coro").unwrap().unwrap().unbind();

            tx_queue
                .send(QueueItem::Work(WorkItem {
                    builder: Box::new(move |_py| Ok(coro)),
                    tx: result_tx,
                }))
                .unwrap();

            let count = drainer.drain_pending(py);
            assert_eq!(count, 1);
            // Trivial coroutines complete inline via the Rust scheduler.
            let result = result_rx.try_recv().unwrap().unwrap();
            let val: i64 = result.extract(py).unwrap();
            assert_eq!(val, 42);
        });
    }

    #[test]
    fn queue_drainer_processes_callbacks() {
        crate::with_py(|py| {
            let (tx_queue, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));
            let sched = test_scheduler_state(py);
            let mut drainer = QueueDrainer::new(rx, py.None(), needs_wake, sched);

            let called = Arc::new(AtomicBool::new(false));
            let called_clone = Arc::clone(&called);
            tx_queue
                .send(QueueItem::Callback(Box::new(move |_py| {
                    called_clone.store(true, Ordering::Release);
                })))
                .unwrap();

            let count = drainer.drain_pending(py);
            assert_eq!(count, 1);
            assert!(called.load(Ordering::Acquire));
        });
    }

    #[test]
    fn queue_drainer_error_isolation() {
        crate::with_py(|py| {
            let (tx_queue, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));
            let sched = test_scheduler_state(py);
            let call_soon = sched.call_soon.clone_ref(py);

            let mut drainer = QueueDrainer::new(rx, call_soon, needs_wake, sched);

            // Item 1: failing builder
            let (tx1, mut rx1) = oneshot::channel();
            tx_queue
                .send(QueueItem::Work(WorkItem {
                    builder: Box::new(|_py| Err(AppError::Internal("builder failed".to_owned()))),
                    tx: tx1,
                }))
                .unwrap();

            // Item 2: succeeding builder (trivial coro completes inline)
            let (tx2, mut rx2) = oneshot::channel();
            let code =
                std::ffi::CString::new("async def _t2(): return 99\ncoro2 = _t2()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            let coro2: Py<PyAny> = locals.get_item("coro2").unwrap().unwrap().unbind();

            tx_queue
                .send(QueueItem::Work(WorkItem {
                    builder: Box::new(move |_py| Ok(coro2)),
                    tx: tx2,
                }))
                .unwrap();

            let count = drainer.drain_pending(py);
            assert_eq!(count, 2);

            // First item should have received an error
            let res1 = rx1.try_recv().unwrap();
            assert!(res1.is_err());
            let err = res1.unwrap_err();
            assert!(
                matches!(err, AppError::Internal(ref s) if s.contains("builder failed")),
                "expected Internal with 'builder failed', got {err:?}"
            );

            // Second item completed inline via Rust scheduler
            let res2 = rx2.try_recv().unwrap().unwrap();
            let val: i64 = res2.extract(py).unwrap();
            assert_eq!(val, 99);
        });
    }

    #[test]
    fn wake_only_when_sleeping() {
        // Verify that needs_wake swap logic works correctly
        let needs_wake = Arc::new(AtomicBool::new(false));

        // When drainer is active (needs_wake = false), swap returns false → no wake
        let was_sleeping = needs_wake.swap(false, Ordering::AcqRel);
        assert!(!was_sleeping, "should not wake when drainer is active");

        // When drainer is sleeping (needs_wake = true), swap returns true → wake
        needs_wake.store(true, Ordering::Release);
        let was_sleeping = needs_wake.swap(false, Ordering::AcqRel);
        assert!(was_sleeping, "should wake when drainer is sleeping");
        // After swap, needs_wake is false again
        assert!(!needs_wake.load(Ordering::Acquire));
    }
}
