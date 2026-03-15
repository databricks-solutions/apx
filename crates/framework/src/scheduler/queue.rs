//! Ready queue for the Rust scheduler.
//!
//! Tasks that become ready (awaitable resolved, event set, timer fired)
//! are pushed here and dispatched to the driver pool via the shared
//! `crossbeam::channel`.

use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use crossbeam_queue::SegQueue;
use pyo3::prelude::*;

use super::task::{SchedulerTask, TaskProxy};
use crate::driver_pool::DriverSender;

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

/// Wake state for the ready queue — sends resumed tasks to the driver channel.
///
/// Set after driver pool construction via [`ReadyQueue::set_wake`].
struct WakeState {
    sender: DriverSender,
}

/// Per-worker ready queue. Lock-free push, dispatches to driver pool.
pub struct ReadyQueue {
    queue: SegQueue<ReadyTask>,
    wake: OnceLock<WakeState>,
    enqueue_count: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for ReadyQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadyQueue")
            .field("has_wake", &self.wake.get().is_some())
            .field("enqueue_count", &self.enqueue_count.load(Ordering::Relaxed))
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
        }
    }

    /// Install the driver sender so that [`push`] dispatches resumed
    /// tasks to the driver pool. Called once after pool construction.
    pub fn set_wake(&self, sender: DriverSender) {
        let _ = self.wake.set(WakeState { sender });
    }

    /// Enqueue a task and send it to the driver pool.
    ///
    /// If the driver sender is not yet installed (before init completes),
    /// falls back to the internal `SegQueue` for later retrieval via [`pop`].
    pub fn push(&self, _py: Python<'_>, task: ReadyTask) {
        self.enqueue_count.fetch_add(1, Ordering::Relaxed);
        if let Some(wake) = self.wake.get() {
            let _ = wake.sender.send_resume(task);
        } else {
            // Fallback before init — buffer locally.
            self.queue.push(task);
        }
    }

    /// Pop the next ready task, if any (fallback path and tests).
    #[cfg(test)]
    pub fn pop(&self) -> Option<ReadyTask> {
        self.queue.pop()
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
