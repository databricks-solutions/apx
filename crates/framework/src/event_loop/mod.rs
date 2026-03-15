//! Persistent asyncio event loop for event-loop-driven ASGI dispatch.
//!
//! One persistent event loop per worker, running `run_forever()` on a
//! dedicated Python thread as a background I/O reactor. Coroutine driving
//! is handled by the [`crate::driver_pool::DriverPool`].
//!
//! This fixes correctness issues with per-request event loops:
//! - `BackgroundTasks` persist after handler returns
//! - `contextvars` are maintained across middleware and handlers
//! - `get_running_loop()` always returns the correct loop

pub mod core;
pub mod handle;
pub mod scheduling;

pub use core::EventLoop;
pub use handle::EventLoopHandle;
