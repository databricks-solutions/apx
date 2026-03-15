//! Unified channel for dispatching work to driver threads.
//!
//! Carries both new coroutine work items (from tokio threads) and resumed
//! tasks (from the event loop thread's `ResumeCallback`). Uses
//! `crossbeam_channel::unbounded()` for lock-free, multi-producer
//! multi-consumer semantics.

use std::fmt;
use std::ops::Not;

use crossbeam_channel::{Receiver, Sender};
use pyo3::prelude::*;

use crate::error::AppError;
use crate::scheduler::queue::ReadyTask;

/// Closure that builds a Python coroutine on a driver thread.
pub type CoroutineBuilder = Box<dyn FnOnce(Python<'_>) -> Result<Py<PyAny>, AppError> + Send>;

/// Work item pushed from tokio threads to driver threads.
pub struct WorkItem {
    /// Builds the coroutine on the driver thread (deferred execution).
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

/// Item dispatched to driver threads.
pub enum DriverItem {
    /// New coroutine from a tokio thread.
    NewWork(WorkItem),
    /// Task resumed after an awaitable resolved (from event loop thread).
    Resume(ReadyTask),
    /// Shutdown sentinel — driver thread should exit.
    Shutdown,
}

impl fmt::Debug for DriverItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NewWork(w) => f.debug_tuple("NewWork").field(w).finish(),
            Self::Resume(r) => f.debug_tuple("Resume").field(r).finish(),
            Self::Shutdown => write!(f, "Shutdown"),
        }
    }
}

/// Send side of the driver channel (cloneable).
///
/// Used by `EventLoopHandle` (for new work) and `ReadyQueue` (for resumed tasks).
#[derive(Clone)]
pub struct DriverSender {
    inner: Sender<DriverItem>,
}

impl fmt::Debug for DriverSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverSender").finish_non_exhaustive()
    }
}

impl DriverSender {
    /// Send a new work item to the driver pool.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel is disconnected (all receivers dropped).
    pub fn send_work(&self, item: WorkItem) -> Result<(), String> {
        self.inner
            .send(DriverItem::NewWork(item))
            .map_err(|_| "driver channel disconnected".to_owned())
    }

    /// Send a resumed task to the driver pool.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel is disconnected.
    pub fn send_resume(&self, task: ReadyTask) -> Result<(), String> {
        self.inner
            .send(DriverItem::Resume(task))
            .map_err(|_| "driver channel disconnected".to_owned())
    }

    /// Send a shutdown sentinel to the driver pool.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel is disconnected.
    pub fn send_shutdown(&self) -> Result<(), String> {
        self.inner
            .send(DriverItem::Shutdown)
            .map_err(|_| "driver channel disconnected".to_owned())
    }
}

/// Receive side of the driver channel (cloneable — shared across driver threads).
#[derive(Clone)]
pub struct DriverReceiver {
    inner: Receiver<DriverItem>,
}

impl fmt::Debug for DriverReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverReceiver").finish_non_exhaustive()
    }
}

impl DriverReceiver {
    /// Block until an item is available (releases GIL when called inside
    /// `py.detach`).
    pub fn recv(&self) -> Result<DriverItem, crossbeam_channel::RecvError> {
        self.inner.recv()
    }
}

/// Create an unbounded driver channel.
pub fn create_driver_channel() -> (DriverSender, DriverReceiver) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (DriverSender { inner: tx }, DriverReceiver { inner: rx })
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
    use tokio::sync::oneshot;

    fn _assert_send<T: Send>() {}

    #[test]
    fn work_item_is_send() {
        _assert_send::<WorkItem>();
    }

    fn _assert_clone<T: Clone>() {}

    #[test]
    fn driver_sender_is_clone() {
        _assert_clone::<DriverSender>();
    }

    #[test]
    fn driver_receiver_is_clone() {
        _assert_clone::<DriverReceiver>();
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
    fn channel_send_recv_shutdown() {
        let (tx, rx) = create_driver_channel();
        tx.send_shutdown().unwrap();
        let item = rx.recv().unwrap();
        assert!(matches!(item, DriverItem::Shutdown));
    }
}
