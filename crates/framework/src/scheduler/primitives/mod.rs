//! Core awaitable primitives for the Rust-driven scheduler.
//!
//! [`Future`] is the foundational awaitable — it implements the Python
//! awaitable protocol so that both asyncio and our Rust coroutine driver
//! can drive it.
//!
//! Additional primitives:
//! - [`Event`] — async event flag (wraps `tokio::sync::Notify`)
//! - [`Timer`] — deadline-based awaitable timer
//! - [`CancelToken`] — structured cancellation flag
//! - [`Lock`] — async mutex (wraps `tokio::sync::Mutex`)
//! - [`Semaphore`] — counting semaphore (wraps `tokio::sync::Semaphore`)
//! - [`BlockingTask`] — awaitable for work spawned on a blocking thread
//! - [`IoHandle`] — stub for future I/O integration

// All types in this module are `#[pyclass]` — PyO3 manages their identity
// semantics, so `Copy` is intentionally not implemented.
#![allow(missing_copy_implementations)]

pub mod blocking;
pub mod event;
pub mod future;
pub mod io;
pub mod sync;
pub mod timer;

// Re-export all public types at the module level for existing consumers.
// Some types are only used as #[pyclass] registrations or will be used by
// future adapter modules — allow unused re-exports.
pub use blocking::BlockingTask;
#[allow(unused_imports)]
pub use blocking::spawn_blocking;
pub use event::{Event, EventWaiter};
pub use future::Future;
#[allow(unused_imports)]
pub use io::IoHandle;
#[allow(unused_imports)]
pub use sync::{
    CancelToken, Lock, LockGuard, LockGuardFuture, Semaphore, SemaphoreAcquire, SemaphorePermit,
};
pub use timer::Timer;
