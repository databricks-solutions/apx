//! Rust-driven coroutine scheduler.
//!
//! Replaces asyncio task scheduling for hot-path primitives while keeping
//! asyncio alive as a fallback for unhandled awaitables.

pub mod adapters;
pub mod driver;
pub mod primitives;
pub mod task;

// Re-export core types used by adapters and integration code.
#[allow(
    unused_imports,
    reason = "will be used by scheduler driver and adapters"
)]
pub use primitives::{
    BlockingTask, CancelToken, IoHandle, RustEvent, RustEventWaiter, RustFuture, RustLock,
    RustLockGuard, RustLockGuardFuture, RustSemaphore, RustSemaphoreAcquire, RustSemaphorePermit,
    Timer, spawn_blocking,
};

#[allow(
    unused_imports,
    reason = "will be used by scheduler integration and adapters"
)]
pub use task::SchedulerTask;
