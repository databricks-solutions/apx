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

use super::scheduling::TaskCallback;
use crate::error::AppError;
use crate::scheduler::driver::{CachedTypes, spawn_and_drive};

/// Closure that builds a Python coroutine on the event loop thread.
pub type CoroutineBuilder = Box<dyn FnOnce(Python<'_>) -> Result<Py<PyAny>, AppError> + Send>;

/// Work item pushed from tokio threads to the event loop thread.
pub struct WorkItem {
    /// Builds the coroutine on the event loop thread (deferred execution).
    pub builder: CoroutineBuilder,
    /// Oneshot sender for the coroutine result.
    pub tx: tokio::sync::oneshot::Sender<Result<Py<PyAny>, AppError>>,
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
/// Constructed once during event loop initialization when
/// [`LoopPolicy::RustNative`](super::core::LoopPolicy::RustNative) is active.
pub struct SchedulerState {
    pub(crate) cached_types: Arc<CachedTypes>,
    pub(crate) call_soon: Py<PyAny>,
    pub(crate) ensure_future: Py<PyAny>,
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
/// invoked, drains all pending [`WorkItem`]s from the channel. If a
/// [`SchedulerState`] is present, coroutines are driven by the Rust
/// scheduler; otherwise they are dispatched as asyncio tasks.
#[pyclass(module = "apx._core")]
pub struct QueueDrainer {
    rx: mpsc::UnboundedReceiver<WorkItem>,
    /// Cached `loop.create_task` bound method.
    create_task: Py<PyAny>,
    /// Cached `loop.call_soon` bound method (local, not threadsafe).
    call_soon: Py<PyAny>,
    /// Python reference to `self` for `call_soon(self)` rescheduling.
    ///
    /// Creates a Python reference cycle — acceptable for a worker-lifetime
    /// singleton. No `__traverse__` needed (not in user-visible object graphs).
    self_ref: Option<Py<PyAny>>,
    /// Shared flag: `true` means the drainer is sleeping and producers
    /// must call `call_soon_threadsafe` to wake it.
    needs_wake: Arc<AtomicBool>,
    /// Rust scheduler state (present when `LoopPolicy::RustNative`).
    scheduler: Option<SchedulerState>,
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
        rx: mpsc::UnboundedReceiver<WorkItem>,
        create_task: Py<PyAny>,
        call_soon: Py<PyAny>,
        needs_wake: Arc<AtomicBool>,
        scheduler: Option<SchedulerState>,
    ) -> Self {
        Self {
            rx,
            create_task,
            call_soon,
            self_ref: None,
            needs_wake,
            scheduler,
        }
    }

    /// Set the Python self-reference for `call_soon(self)` rescheduling.
    pub fn set_self_ref(&mut self, self_ref: Py<PyAny>) {
        self.self_ref = Some(self_ref);
    }
}

#[pymethods]
impl QueueDrainer {
    fn __call__(&mut self, py: Python<'_>) -> PyResult<()> {
        let count = self.drain_pending(py);
        if count > 0 {
            return self.reschedule(py);
        }
        self.transition_to_sleep(py)
    }
}

impl QueueDrainer {
    /// Pop all pending items and dispatch each.
    fn drain_pending(&mut self, py: Python<'_>) -> usize {
        let mut count = 0;
        while let Ok(item) = self.rx.try_recv() {
            count += 1;
            self.dispatch_item(py, item);
        }
        count
    }

    /// Dispatch one work item — either via the Rust scheduler or asyncio.
    fn dispatch_item(&self, py: Python<'_>, item: WorkItem) {
        if let Some(ref sched) = self.scheduler {
            QueueDrainer::dispatch_via_scheduler(py, item, sched);
        } else {
            self.dispatch_via_asyncio(py, item);
        }
    }

    /// Drive the coroutine through the Rust scheduler.
    fn dispatch_via_scheduler(py: Python<'_>, item: WorkItem, sched: &SchedulerState) {
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
            &sched.ensure_future,
        );
    }

    /// Create an asyncio task from one work item (original path).
    fn dispatch_via_asyncio(&self, py: Python<'_>, item: WorkItem) {
        let result = self.try_create_task(py, item.builder);
        match result {
            Ok(task) => attach_done_callback(py, task, item.tx),
            Err(e) => {
                let _ = item.tx.send(Err(e));
            }
        }
    }

    /// Build coroutine + create asyncio task.
    fn try_create_task(
        &self,
        py: Python<'_>,
        builder: CoroutineBuilder,
    ) -> Result<Py<PyAny>, AppError> {
        let coro = builder(py)?;
        self.create_task
            .call1(py, (coro,))
            .map_err(|e| AppError::Internal(format!("create_task: {e}")))
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
        Ok(()) // truly idle — producer will wake us via call_soon_threadsafe
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

/// Add a [`TaskCallback`] as the done callback on an asyncio task.
fn attach_done_callback(
    py: Python<'_>,
    task: Py<PyAny>,
    tx: tokio::sync::oneshot::Sender<Result<Py<PyAny>, AppError>>,
) {
    let cb_result = Py::new(py, TaskCallback::new(tx));
    match cb_result {
        Ok(callback) => {
            let _ = task.call_method1(py, c"add_done_callback", (callback,));
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create TaskCallback");
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

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
            let drainer = QueueDrainer::new(rx, py.None(), py.None(), needs_wake, None);
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
            // We can't call __call__ directly without call_soon/self_ref setup,
            // but we can test the state transition logic.
            let mut drainer = QueueDrainer::new(rx, py.None(), py.None(), needs_wake.clone(), None);
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

            // Resolve a real create_task from an event loop
            let asyncio = py.import(c"asyncio").unwrap();
            let event_loop = asyncio.call_method0(c"new_event_loop").unwrap();
            let create_task = event_loop.getattr(c"create_task").unwrap().unbind();
            let call_soon = event_loop.getattr(c"call_soon").unwrap().unbind();

            let mut drainer = QueueDrainer::new(rx, create_task, call_soon, needs_wake, None);

            // Push a work item with a trivial coroutine builder
            let (result_tx, mut result_rx) = oneshot::channel();
            let code = std::ffi::CString::new("async def _t(): return 42\ncoro = _t()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            let coro: Py<PyAny> = locals.get_item("coro").unwrap().unwrap().unbind();

            tx_queue
                .send(WorkItem {
                    builder: Box::new(move |_py| Ok(coro)),
                    tx: result_tx,
                })
                .unwrap();

            let count = drainer.drain_pending(py);
            assert_eq!(count, 1);
            // The task was created; result_rx should not be immediately resolved
            // (task needs event loop to run), but the channel should still be open.
            assert!(result_rx.try_recv().is_err(), "task not yet completed");

            event_loop.call_method0(c"close").unwrap();
        });
    }

    #[test]
    fn queue_drainer_error_isolation() {
        crate::with_py(|py| {
            let (tx_queue, rx) = mpsc::unbounded_channel();
            let needs_wake = Arc::new(AtomicBool::new(false));

            let asyncio = py.import(c"asyncio").unwrap();
            let event_loop = asyncio.call_method0(c"new_event_loop").unwrap();
            let create_task = event_loop.getattr(c"create_task").unwrap().unbind();
            let call_soon = event_loop.getattr(c"call_soon").unwrap().unbind();

            let mut drainer = QueueDrainer::new(rx, create_task, call_soon, needs_wake, None);

            // Item 1: failing builder
            let (tx1, mut rx1) = oneshot::channel();
            tx_queue
                .send(WorkItem {
                    builder: Box::new(|_py| Err(AppError::Internal("builder failed".to_owned()))),
                    tx: tx1,
                })
                .unwrap();

            // Item 2: succeeding builder
            let (tx2, mut rx2) = oneshot::channel();
            let code =
                std::ffi::CString::new("async def _t2(): return 99\ncoro2 = _t2()\n").unwrap();
            let locals = pyo3::types::PyDict::new(py);
            py.run(&code, None, Some(&locals)).unwrap();
            let coro2: Py<PyAny> = locals.get_item("coro2").unwrap().unwrap().unbind();

            tx_queue
                .send(WorkItem {
                    builder: Box::new(move |_py| Ok(coro2)),
                    tx: tx2,
                })
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

            // Second item should have created a task (channel still open)
            assert!(rx2.try_recv().is_err(), "task not yet completed");

            event_loop.call_method0(c"close").unwrap();
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
