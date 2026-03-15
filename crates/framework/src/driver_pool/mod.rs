//! Two-stage driver pool for coroutine scope construction.
//!
//! **Stage 1**: N driver threads consume [`WorkItem`]s from a shared
//! `crossbeam::channel`, acquire the GIL briefly to build scope dicts
//! (Python coroutine objects), then push [`ReadyCoro`]s to stage 2.
//!
//! **Stage 2**: The event loop thread's [`QueueDrainer`](super::event_loop::queue::QueueDrainer)
//! consumes ready coroutines and drives them via the Rust scheduler.
//! This avoids GIL starvation — only the event loop thread steps coroutines.

mod channel;
mod pool;
mod thread;

pub use channel::{DriverSender, ReadyCoroReceiver, WorkItem, create_ready_coro_channel};

// Re-exported for test modules in other crates/modules.
#[cfg(test)]
pub use channel::ReadyCoro;
pub use pool::DriverPool;
pub use thread::SharedDriverState;
