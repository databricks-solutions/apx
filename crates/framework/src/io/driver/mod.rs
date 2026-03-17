//! Coroutine driver — the core innovation of the Rust scheduler.
//!
//! Replaces `asyncio.Task._step` by calling `coro.send(value)` directly from
//! Rust and interpreting yielded values to decide what to do next.
//!
//! The most common case (36% of all primitive calls) is `yield None` — the
//! driver handles this by looping immediately without any
//! Python→asyncio→Python round trip.

pub mod ffi;
pub mod primitives;
pub mod task;

use pyo3::prelude::*;

use self::ffi::{AwaitableKind, CoroutineOps, StepResult};
use self::task::SchedulerTask;

// ---------------------------------------------------------------------------
// DriveStats — per-request scheduler drive counters
// ---------------------------------------------------------------------------

/// Counters collected during a single `drive_task` invocation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DriveStats {
    pub steps: u32,
    pub yield_none: u32,
    pub yield_future: u32,
    pub yield_asyncio_future: u32,
    pub yield_coroutine: u32,
    pub yield_unknown: u32,
    pub budget_exhausted: bool,
}

// ---------------------------------------------------------------------------
// DriveResult — outcome of driving a task
// ---------------------------------------------------------------------------

/// Outcome of driving a [`SchedulerTask`] until it suspends or completes.
#[derive(Debug)]
pub enum DriveResult {
    /// Task completed with a value.
    Completed(Py<PyAny>),
    /// Task raised an exception.
    Error(PyErr),
    /// Waiting on a `Future` — resume when it resolves.
    WaitingOnFuture(Py<PyAny>),
    /// Waiting on a `EventWaiter` — resume when event is set.
    WaitingOnEvent(Py<PyAny>),
    /// Waiting on an `asyncio.Future` — attach done callback.
    WaitingOnAsyncioFuture(Py<PyAny>),
    /// Step budget exhausted — re-enqueue for fairness.
    BudgetExhausted,
}

/// Maximum `YieldNone` steps before re-enqueueing for fairness.
pub const DEFAULT_STEP_BUDGET: usize = 128;

// ---------------------------------------------------------------------------
// drive_task — the main drive loop
// ---------------------------------------------------------------------------

/// Drive a [`SchedulerTask`] until it suspends or completes.
///
/// This is the hot loop. When the yielded object is `None` (most common case),
/// it immediately loops without suspending. Sub-coroutines are pushed onto the
/// task's coroutine stack and driven inline.
#[expect(
    clippy::needless_continue,
    reason = "explicit `continue` documents intent to re-enter the drive loop"
)]
pub fn drive_task(
    py: Python<'_>,
    task: &mut SchedulerTask,
    ops: &dyn CoroutineOps,
    step_budget: usize,
) -> (DriveResult, DriveStats) {
    let mut stats = DriveStats::default();
    loop {
        // Clone the coro ref so the immutable borrow on `task` is released
        // before we mutate it (take_throw_error / take_send_value).
        let coro = match task.active_coro() {
            Ok(c) => c.clone_ref(py),
            Err(e) => return (DriveResult::Error(e), stats),
        };
        let step_result = if let Some(err) = task.take_throw_error() {
            ops.step_throw(py, &coro, err)
        } else {
            let send_val = task.take_send_value();
            ops.step(py, &coro, send_val.as_ref())
        };

        match step_result {
            StepResult::Yielded(obj) => {
                let kind = ops.classify(py, &obj);
                match kind {
                    AwaitableKind::YieldNone => {
                        stats.steps += 1;
                        stats.yield_none += 1;
                        if stats.steps as usize >= step_budget {
                            stats.budget_exhausted = true;
                            return (DriveResult::BudgetExhausted, stats);
                        }
                        continue;
                    }
                    AwaitableKind::Future => {
                        stats.yield_future += 1;
                        return (DriveResult::WaitingOnFuture(obj), stats);
                    }
                    AwaitableKind::EventWaiter => {
                        return (DriveResult::WaitingOnEvent(obj), stats);
                    }
                    AwaitableKind::Coroutine => {
                        stats.yield_coroutine += 1;
                        task.push_coro(obj);
                        continue; // drive the sub-coroutine
                    }
                    AwaitableKind::AsyncioFuture => {
                        stats.yield_asyncio_future += 1;
                        return (DriveResult::WaitingOnAsyncioFuture(obj), stats);
                    }
                    AwaitableKind::CustomAwaitable => {
                        // Call __await__() to get the iterator, push onto coro stack.
                        match obj.call_method0(py, c"__await__") {
                            Ok(iter) => {
                                stats.yield_coroutine += 1;
                                task.push_coro(iter);
                                continue;
                            }
                            Err(e) => return (DriveResult::Error(e), stats),
                        }
                    }
                    AwaitableKind::Unknown => {
                        stats.yield_unknown += 1;
                        let type_name = obj
                            .bind(py)
                            .get_type()
                            .name()
                            .map_or_else(|_| "<unknown>".to_owned(), |n| n.to_string());
                        return (
                            DriveResult::Error(pyo3::exceptions::PyTypeError::new_err(format!(
                                "unsupported awaitable type yielded: {type_name}"
                            ))),
                            stats,
                        );
                    }
                }
            }
            StepResult::Completed(value) => {
                if task.pop_coro() {
                    // Sub-coroutine completed — send its result to parent.
                    task.set_send_value(value);
                    continue;
                }
                // Top-level coroutine completed.
                return (DriveResult::Completed(value), stats);
            }
            StepResult::Error(e) => {
                if task.pop_coro() {
                    // Sub-coroutine raised — throw into parent.
                    task.set_throw_error(e);
                    continue;
                }
                return (DriveResult::Error(e), stats);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// first_drive — inline completion (test-only, kept for scheduler unit tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
use self::ffi::ContextGuard;
#[cfg(test)]
use crate::protocol::http::error::AppError;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::oneshot;

/// Outcome of driving a coroutine's first cycle on the driver thread.
#[cfg(test)]
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "fields used for pattern matching and Debug output in tests"
)]
pub enum FirstDriveOutcome {
    /// Completed or errored inline — result already sent via `result_tx`.
    Inline,
    /// Suspended on an awaitable — event loop thread must attach continuation.
    Suspended {
        task: Box<SchedulerTask>,
        sched_task: Option<Py<PyAny>>,
        drive_result: DriveResult,
    },
}

/// Drive a coroutine's first cycle. Completes trivial coros inline;
/// returns suspended state for the event loop to handle.
///
/// Skips task registration — drive mechanics don't depend on it.
#[cfg(test)]
pub fn first_drive(
    py: Python<'_>,
    coro: Py<PyAny>,
    result_tx: oneshot::Sender<Result<Py<PyAny>, AppError>>,
    ops: &Arc<dyn CoroutineOps>,
) -> FirstDriveOutcome {
    let mut task = match SchedulerTask::new(py, coro, result_tx) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "first_drive: task creation failed");
            return FirstDriveOutcome::Inline;
        }
    };
    let ctx_guard = task
        .ctx
        .as_ref()
        .map(|c| c.clone_ref(py))
        .and_then(ContextGuard::enter);
    let (drive_result, _stats) = drive_task(py, &mut task, ops.as_ref(), DEFAULT_STEP_BUDGET);
    drop(ctx_guard);
    route_first_drive(py, task, drive_result)
}

/// Route the drive result: complete inline or return suspended state.
#[cfg(test)]
fn route_first_drive(
    _py: Python<'_>,
    mut task: SchedulerTask,
    drive_result: DriveResult,
) -> FirstDriveOutcome {
    match drive_result {
        DriveResult::Completed(value) => {
            complete_inline(&mut task, Ok(value));
            FirstDriveOutcome::Inline
        }
        DriveResult::Error(err) => {
            complete_inline(&mut task, Err(AppError::Internal(err.to_string())));
            FirstDriveOutcome::Inline
        }
        result => FirstDriveOutcome::Suspended {
            task: Box::new(task),
            sched_task: None,
            drive_result: result,
        },
    }
}

/// Send result through the oneshot channel for inline completion.
#[cfg(test)]
fn complete_inline(task: &mut SchedulerTask, result: Result<Py<PyAny>, AppError>) {
    if let Some(tx) = task.take_result_tx() {
        let _ = tx.send(result);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::ffi::FfiCoroutineOps;
    use super::*;

    // -- first_drive tests -----------------------------------------------

    #[test]
    fn first_drive_trivial_coroutine() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(c"async def _f(): return 42", None, None).unwrap();
            let coro = py.eval(c"_f()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn first_drive_coroutine_error() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(c"async def _err(): raise ValueError('boom')", None, None)
                .unwrap();
            let coro = py.eval(c"_err()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

            let result = rx.try_recv().unwrap();
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(
                matches!(err, AppError::Internal(ref s) if s.contains("boom")),
                "expected Internal('boom'), got {err:?}"
            );
        });
    }

    #[test]
    fn first_drive_suspended_coroutine() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            // Create a coroutine that suspends on an asyncio.Future.
            let asyncio = py.import(c"asyncio").unwrap();
            let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
            let events = py.import(c"asyncio.events").unwrap();
            let _ = events.call_method1(c"_set_running_loop", (&loop_obj,));

            py.run(
                c"
import asyncio
async def _suspend():
    loop = asyncio.get_running_loop()
    fut = loop.create_future()
    await fut
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"_suspend()", None, None).unwrap().unbind();

            let (tx, _rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(
                matches!(outcome, FirstDriveOutcome::Suspended { .. }),
                "expected Suspended, got {outcome:?}"
            );

            let _ = events.call_method1(c"_set_running_loop", (py.None(),));
            let _ = loop_obj.call_method0(c"close");
        });
    }

    #[test]
    fn test_contextvars_propagation() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            // Set a contextvar before first_drive, verify it's visible
            // inside the coroutine.
            py.run(
                c"
import contextvars
test_var = contextvars.ContextVar('test_var', default='unset')
test_var.set('hello_from_middleware')

async def _check_ctx():
    return test_var.get()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"_check_ctx()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            assert!(matches!(outcome, FirstDriveOutcome::Inline));

            let result = rx.try_recv().unwrap().unwrap();
            let val: String = result.extract(py).unwrap();
            assert_eq!(val, "hello_from_middleware");
        });
    }

    #[test]
    fn drive_custom_awaitable() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(
                c"
class CustomAwaitable:
    def __await__(self):
        yield None  # suspend once
        return 42

async def uses_custom():
    return await CustomAwaitable()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"uses_custom()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            // CustomAwaitable yields None once, so the driver suspends.
            assert!(
                matches!(outcome, FirstDriveOutcome::Inline),
                "expected Inline (yield None loops then __await__ returns), got {outcome:?}"
            );
            let result = rx.try_recv().unwrap().unwrap();
            let num: i64 = result.extract(py).unwrap();
            assert_eq!(num, 42);
        });
    }

    #[test]
    fn custom_awaitable_bad_await_raises() {
        crate::with_py(|py| {
            let ops: Arc<dyn CoroutineOps> = Arc::new(FfiCoroutineOps::resolve(py).unwrap());

            py.run(
                c"
class BadAwait:
    def __await__(self):
        return 42  # not an iterator

async def uses_bad():
    return await BadAwait()
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"uses_bad()", None, None).unwrap().unbind();

            let (tx, mut rx) = oneshot::channel();
            let outcome = first_drive(py, coro, tx, &ops);
            // Should complete inline with an error (TypeError from non-iterator).
            assert!(matches!(outcome, FirstDriveOutcome::Inline));
            let result = rx.try_recv().unwrap();
            assert!(result.is_err(), "expected error from bad __await__, got Ok");
        });
    }
}
