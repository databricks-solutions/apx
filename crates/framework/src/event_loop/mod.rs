//! Event loop management for worker processes.
//!
//! Provides [`InlineEventLoop`] — a single-thread asyncio event loop that
//! runs dormant while the Rust scheduler drives coroutines inline on the
//! tokio thread.

pub mod core;
pub mod inline;

pub use inline::InlineEventLoop;
