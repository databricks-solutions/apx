//! Unified channel for dispatching work to driver threads.
//!
//! **Stage 1** (ch1): tokio → driver thread. Carries [`WorkItem`]s (coroutine
//! builders) via `crossbeam_channel::unbounded()`. Driver threads build scope
//! dicts and produce ready coroutines.
//!
//! **Stage 2** (ch2): driver thread → event loop. Carries [`ReadyCoro`]s
//! (pre-built coro + result sender) via a second `crossbeam_channel::unbounded()`.
//! The event loop thread's `QueueDrainer` consumes these and drives coroutines.

use std::fmt;
use std::ops::Not;

use crossbeam_channel::{Receiver, Sender};
use pyo3::prelude::*;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Stage 1: tokio → driver thread
// ---------------------------------------------------------------------------

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

/// Item dispatched to driver threads (stage 1).
pub enum DriverItem {
    /// New coroutine from a tokio thread.
    NewWork(WorkItem),
    /// Shutdown sentinel — driver thread should exit.
    Shutdown,
}

impl fmt::Debug for DriverItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NewWork(w) => f.debug_tuple("NewWork").field(w).finish(),
            Self::Shutdown => write!(f, "Shutdown"),
        }
    }
}

/// Send side of the driver channel (cloneable).
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
    pub fn send_work(&self, item: WorkItem) -> Result<(), String> {
        self.inner
            .send(DriverItem::NewWork(item))
            .map_err(|_| "driver channel disconnected".to_owned())
    }

    /// Send a shutdown sentinel to the driver pool.
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

/// Create an unbounded driver channel (stage 1).
pub fn create_driver_channel() -> (DriverSender, DriverReceiver) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (DriverSender { inner: tx }, DriverReceiver { inner: rx })
}

// ---------------------------------------------------------------------------
// Stage 2: driver thread → event loop thread
// ---------------------------------------------------------------------------

/// A pre-built coroutine ready to be driven on the event loop thread.
pub struct ReadyCoro {
    /// The Python coroutine (already built by the driver thread).
    pub coro: Py<PyAny>,
    /// Oneshot sender for the coroutine result.
    pub tx: tokio::sync::oneshot::Sender<Result<Py<PyAny>, AppError>>,
}

impl fmt::Debug for ReadyCoro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadyCoro")
            .field("pending", &self.tx.is_closed().not())
            .finish_non_exhaustive()
    }
}

/// Send side of the stage-2 channel (driver → event loop).
#[derive(Clone)]
pub struct ReadyCoroSender {
    inner: Sender<ReadyCoro>,
}

impl fmt::Debug for ReadyCoroSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadyCoroSender").finish_non_exhaustive()
    }
}

impl ReadyCoroSender {
    /// Send a ready coroutine to the event loop thread.
    pub fn send(&self, coro: ReadyCoro) -> Result<(), crossbeam_channel::SendError<ReadyCoro>> {
        self.inner.send(coro)
    }
}

/// Receive side of the stage-2 channel (consumed by QueueDrainer).
pub struct ReadyCoroReceiver {
    inner: Receiver<ReadyCoro>,
}

impl fmt::Debug for ReadyCoroReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadyCoroReceiver").finish_non_exhaustive()
    }
}

impl ReadyCoroReceiver {
    /// Try to receive a ready coroutine without blocking.
    pub fn try_recv(&self) -> Result<ReadyCoro, crossbeam_channel::TryRecvError> {
        self.inner.try_recv()
    }
}

/// Create an unbounded stage-2 channel (driver → event loop).
pub fn create_ready_coro_channel() -> (ReadyCoroSender, ReadyCoroReceiver) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (
        ReadyCoroSender { inner: tx },
        ReadyCoroReceiver { inner: rx },
    )
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
    fn ready_coro_sender_is_clone() {
        _assert_clone::<ReadyCoroSender>();
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

    #[test]
    fn ready_coro_channel_send_recv() {
        crate::with_py(|py| {
            let (tx_ch, rx_ch) = create_ready_coro_channel();
            let (result_tx, _result_rx) = oneshot::channel();
            tx_ch
                .send(ReadyCoro {
                    coro: py.None(),
                    tx: result_tx,
                })
                .unwrap();
            let ready = rx_ch.try_recv().unwrap();
            assert!(!ready.tx.is_closed());
        });
    }
}
