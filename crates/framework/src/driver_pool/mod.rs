//! Multi-worker driver pool for coroutine execution.
//!
//! Replaces the single-thread [`QueueDrainer`] with N driver threads that
//! consume work from a shared `crossbeam::channel`. Each thread acquires
//! the GIL only when actively stepping a coroutine and releases it while
//! blocking on the work channel via `py.detach(|| receiver.recv())`.
//!
//! The asyncio event loop thread becomes a background I/O reactor only —
//! `ResumeCallback` fires there when asyncio.Futures resolve, pushing
//! tasks through the channel to driver threads.

mod channel;
mod pool;
mod thread;

pub use channel::{DriverSender, WorkItem};
pub use pool::DriverPool;
pub use thread::SharedDriverState;
