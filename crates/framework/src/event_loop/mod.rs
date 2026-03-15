//! Persistent asyncio event loop with hybrid two-stage dispatch.
//!
//! One persistent event loop per worker, running `run_forever()` on a
//! dedicated Python thread. Driver threads build scope dicts (stage 1),
//! then the event loop thread drives coroutines (stage 2) via
//! [`queue::QueueDrainer`].
//!
//! This fixes correctness issues with per-request event loops:
//! - `BackgroundTasks` persist after handler returns
//! - `contextvars` are maintained across middleware and handlers
//! - `get_running_loop()` always returns the correct loop

pub mod core;
pub mod handle;
pub mod queue;
pub mod scheduling;
pub mod wake;

pub use core::EventLoop;
pub use handle::EventLoopHandle;
