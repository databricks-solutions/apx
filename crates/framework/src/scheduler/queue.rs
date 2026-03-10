//! Ready queue for the Rust scheduler.
//!
//! Tasks that become ready (awaitable resolved, event set, timer fired)
//! are pushed here. The drain loop in [`super::super::event_loop::queue::QueueDrainer`]
//! pops and re-drives them.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_queue::SegQueue;
use pyo3::prelude::*;

use super::driver::{CachedTypes, resume_task};
use super::task::{SchedulerTask, TaskProxy};

/// A task ready to be re-driven by the scheduler.
///
/// `result_tx` lives inside [`SchedulerTask`] — not here.
pub struct ReadyTask {
    /// The task to resume.
    pub task: SchedulerTask,
    /// Optional proxy installed as `asyncio.current_task()` during driving.
    pub proxy: Option<Py<TaskProxy>>,
}

impl std::fmt::Debug for ReadyTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadyTask")
            .field("task", &self.task)
            .finish()
    }
}

/// Wake state for the ready queue — schedules the drainer when it is sleeping.
///
/// Set after [`super::super::event_loop::queue::QueueDrainer`] construction
/// via [`ReadyQueue::set_wake`].
struct WakeState {
    needs_wake: Arc<AtomicBool>,
    call_soon: Py<PyAny>,
    drainer_ref: Py<PyAny>,
}

/// Per-worker ready queue. Lock-free push, single-consumer drain.
pub struct ReadyQueue {
    queue: SegQueue<ReadyTask>,
    wake: OnceLock<WakeState>,
    enqueue_count: std::sync::atomic::AtomicU64,
    drain_count: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for ReadyQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadyQueue")
            .field("has_wake", &self.wake.get().is_some())
            .field("enqueue_count", &self.enqueue_count.load(Ordering::Relaxed))
            .field("drain_count", &self.drain_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl ReadyQueue {
    /// Create an empty ready queue (wake state set later via [`set_wake`]).
    pub fn new() -> Self {
        Self {
            queue: SegQueue::new(),
            wake: OnceLock::new(),
            enqueue_count: std::sync::atomic::AtomicU64::new(0),
            drain_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Install the wake references so that [`push`] can reschedule the
    /// drainer when it is sleeping. Called once after drainer allocation.
    pub fn set_wake(
        &self,
        needs_wake: Arc<AtomicBool>,
        call_soon: Py<PyAny>,
        drainer_ref: Py<PyAny>,
    ) {
        let _ = self.wake.set(WakeState {
            needs_wake,
            call_soon,
            drainer_ref,
        });
    }

    /// Enqueue a task and wake the drainer if it is sleeping.
    pub fn push(&self, py: Python<'_>, task: ReadyTask) {
        self.enqueue_count.fetch_add(1, Ordering::Relaxed);
        self.queue.push(task);
        if let Some(wake) = self.wake.get()
            && wake.needs_wake.swap(false, Ordering::AcqRel)
        {
            let _ = wake.call_soon.call1(py, (&wake.drainer_ref,));
        }
    }

    /// Pop the next ready task, if any.
    pub fn pop(&self) -> Option<ReadyTask> {
        self.queue.pop()
    }

    /// Drain all ready tasks on the current thread.
    ///
    /// Returns the number of tasks drained. Tasks re-enqueued during
    /// driving (e.g. by `handle_drive_result`) are picked up in the
    /// same drain cycle.
    pub fn drain(
        &self,
        py: Python<'_>,
        cached_types: &Arc<CachedTypes>,
        call_soon: &Py<PyAny>,
        ensure_future: &Py<PyAny>,
        ready_queue: &Arc<ReadyQueue>,
    ) -> usize {
        let mut count = 0;
        while let Some(ready) = self.pop() {
            count += 1;
            if let Err(e) = resume_task(
                py,
                ready,
                cached_types,
                call_soon,
                ensure_future,
                ready_queue,
            ) {
                tracing::warn!(error = %e, "ready queue drain: resume failed");
            }
        }
        if count > 0 {
            self.drain_count.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(
                batch_size = count,
                total_enqueued = self.enqueue_count.load(Ordering::Relaxed),
                "ready_queue_drain"
            );
        }
        count
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
#[expect(
    clippy::used_underscore_items,
    reason = "test code uses underscore for clarity"
)]
mod tests {
    use super::*;

    fn _assert_send<T: Send>() {}

    #[test]
    fn ready_task_is_send() {
        _assert_send::<ReadyTask>();
    }

    #[test]
    fn ready_queue_push_pop() {
        crate::with_py(|py| {
            let queue = ReadyQueue::new();
            assert!(queue.pop().is_none());

            let (tx, _rx) = tokio::sync::oneshot::channel();
            let task = SchedulerTask::new(py, py.None(), tx).unwrap();
            queue.push(py, ReadyTask { task, proxy: None });

            let ready = queue.pop();
            assert!(ready.is_some());
            assert!(queue.pop().is_none());
        });
    }
}
