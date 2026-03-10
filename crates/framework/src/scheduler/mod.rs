//! Rust-driven coroutine scheduler.
//!
//! Replaces asyncio task scheduling for hot-path primitives while keeping
//! asyncio alive as a fallback for unhandled awaitables.

pub mod adapters;
pub mod driver;
pub mod primitives;
pub mod task;
