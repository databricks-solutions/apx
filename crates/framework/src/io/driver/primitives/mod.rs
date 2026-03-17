//! Core awaitable primitives for the Rust-driven scheduler.
//!
//! [`Future`] is the foundational awaitable — it implements the Python
//! awaitable protocol so that both asyncio and our Rust coroutine driver
//! can drive it.
//!
//! Additional primitives:
//! - [`Event`] — async event flag (wraps `tokio::sync::Notify`)

pub mod event;
pub mod future;

pub use event::{Event, EventWaiter};
pub use future::Future;
