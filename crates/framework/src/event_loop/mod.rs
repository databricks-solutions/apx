//! Persistent asyncio event loop for event-loop-driven ASGI dispatch.
//!
//! One persistent event loop per worker, running `run_forever()` on a
//! dedicated Python thread. Handler coroutines are submitted via
//! `call_soon_threadsafe` and driven natively by asyncio.
//!
//! This fixes correctness issues with per-request event loops:
//! - `BackgroundTasks` persist after handler returns
//! - `contextvars` are maintained across middleware and handlers
//! - `get_running_loop()` always returns the correct loop

pub mod core;
pub mod handle;
pub mod queue;
pub mod scheduling;
mod wake;

pub use core::EventLoop;
pub use handle::EventLoopHandle;
