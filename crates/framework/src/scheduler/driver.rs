//! Coroutine driver — the core innovation of the Rust scheduler.
//!
//! Replaces `asyncio.Task._step` by calling `coro.send(value)` directly from
//! Rust and interpreting yielded values to decide what to do next.
//!
//! The most common case (36% of all primitive calls) is `yield None` — the
//! driver handles this by looping immediately without any
//! Python→asyncio→Python round trip.

use pyo3::PyTypeInfo;
use pyo3::prelude::*;
use pyo3::types::PyType;

use super::primitives::{RustEventWaiter, RustFuture};
use super::task::SchedulerTask;

// ---------------------------------------------------------------------------
// CachedTypes — pre-resolved Python type references
// ---------------------------------------------------------------------------

/// Pre-resolved Python type references, cached at startup to avoid repeated
/// `import` / `getattr` calls in the hot path.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub struct CachedTypes {
    /// `asyncio.Future`
    pub asyncio_future: Py<PyType>,
    /// `asyncio.Task`
    pub asyncio_task: Py<PyType>,
    /// `types.CoroutineType`
    pub coroutine_type: Py<PyType>,
    /// `types.GeneratorType`
    pub generator_type: Py<PyType>,
    /// Our `RustFuture` type object.
    pub rust_future_type: Py<PyType>,
    /// Our `RustEventWaiter` type object.
    pub rust_event_waiter_type: Py<PyType>,
}

impl CachedTypes {
    /// Resolve all Python types once at startup.
    #[allow(
        dead_code,
        reason = "will be used by scheduler integration in a future phase"
    )]
    pub fn resolve(py: Python<'_>) -> PyResult<Self> {
        let asyncio = py.import(c"asyncio")?;
        let types = py.import(c"types")?;

        Ok(Self {
            asyncio_future: asyncio.getattr(c"Future")?.cast_into::<PyType>()?.unbind(),
            asyncio_task: asyncio.getattr(c"Task")?.cast_into::<PyType>()?.unbind(),
            coroutine_type: types
                .getattr(c"CoroutineType")?
                .cast_into::<PyType>()?
                .unbind(),
            generator_type: types
                .getattr(c"GeneratorType")?
                .cast_into::<PyType>()?
                .unbind(),
            rust_future_type: RustFuture::type_object(py).unbind(),
            rust_event_waiter_type: RustEventWaiter::type_object(py).unbind(),
        })
    }
}

// ---------------------------------------------------------------------------
// StepResult — outcome of advancing a coroutine one step
// ---------------------------------------------------------------------------

/// Outcome of a single `send` / `throw` cycle on a coroutine.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub enum StepResult {
    /// Coroutine yielded a value (needs classification).
    Yielded(Py<PyAny>),
    /// Coroutine completed (raised `StopIteration` with a value).
    Completed(Py<PyAny>),
    /// Coroutine raised an exception.
    Error(PyErr),
}

// ---------------------------------------------------------------------------
// step / step_throw — single send/throw cycle
// ---------------------------------------------------------------------------

/// Advance a coroutine one step by calling `coro.send(value)`.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub fn step(
    py: Python<'_>,
    coro: &Bound<'_, PyAny>,
    value: Option<&Bound<'_, PyAny>>,
) -> StepResult {
    let send_val = value.map_or_else(|| py.None().into_bound(py), |v| v.clone());
    match coro.call_method1(c"send", (&send_val,)) {
        Ok(yielded) => StepResult::Yielded(yielded.unbind()),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) => {
            let value = e
                .value(py)
                .getattr(c"value")
                .map_or_else(|_| py.None(), |v| v.unbind());
            StepResult::Completed(value)
        }
        Err(e) => StepResult::Error(e),
    }
}

/// Advance a coroutine one step by calling `coro.throw(exc)`.
///
/// Python 3.12+ accepts just the exception instance.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub fn step_throw(py: Python<'_>, coro: &Bound<'_, PyAny>, err: PyErr) -> StepResult {
    match coro.call_method1(c"throw", (err.value(py),)) {
        Ok(yielded) => StepResult::Yielded(yielded.unbind()),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) => {
            let value = e
                .value(py)
                .getattr(c"value")
                .map_or_else(|_| py.None(), |v| v.unbind());
            StepResult::Completed(value)
        }
        Err(e) => StepResult::Error(e),
    }
}

// ---------------------------------------------------------------------------
// AwaitableKind — classification of yielded objects
// ---------------------------------------------------------------------------

/// Classification of a yielded object from a coroutine.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub enum AwaitableKind {
    /// Our own `RustFuture` — poll directly via `__next__`.
    RustFuture,
    /// Our own `RustEventWaiter` — check event flag.
    RustEventWaiter,
    /// `asyncio.Future` (not Task) — attach done callback.
    AsyncioFuture,
    /// A coroutine object — push onto coroutine stack.
    Coroutine,
    /// `yield None` — reschedule immediately (like `call_soon`).
    YieldNone,
    /// Unknown awaitable — fall back to `asyncio.ensure_future`.
    Unknown,
}

// ---------------------------------------------------------------------------
// classify — inspects a yielded object
// ---------------------------------------------------------------------------

/// Classify a yielded value to determine what the driver should do next.
///
/// Check order is optimised for the common case: `yield None` first (36% of
/// all calls), then our own types, then coroutines, then asyncio futures.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub fn classify(py: Python<'_>, yielded: &Bound<'_, PyAny>, types: &CachedTypes) -> AwaitableKind {
    // Fast path: yield None is the most common case.
    if yielded.is_none() {
        return AwaitableKind::YieldNone;
    }

    // Check our own types first (fast isinstance checks).
    if yielded
        .is_instance(types.rust_future_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::RustFuture;
    }
    if yielded
        .is_instance(types.rust_event_waiter_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::RustEventWaiter;
    }

    // Check for coroutine (sub-coroutine yield).
    if yielded
        .is_instance(types.coroutine_type.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::Coroutine;
    }

    // Check for asyncio.Future (includes asyncio.Task since Task extends Future).
    if yielded
        .is_instance(types.asyncio_future.bind(py).as_any())
        .unwrap_or(false)
    {
        return AwaitableKind::AsyncioFuture;
    }

    AwaitableKind::Unknown
}

// ---------------------------------------------------------------------------
// DriveResult — outcome of driving a task
// ---------------------------------------------------------------------------

/// Outcome of driving a [`SchedulerTask`] until it suspends or completes.
#[allow(
    dead_code,
    reason = "will be used by scheduler integration in a future phase"
)]
pub enum DriveResult {
    /// Task completed with a value.
    Completed(Py<PyAny>),
    /// Task raised an exception.
    Error(PyErr),
    /// Waiting on a `RustFuture` — resume when it resolves.
    WaitingOnRustFuture(Py<PyAny>),
    /// Waiting on a `RustEventWaiter` — resume when event is set.
    WaitingOnEvent(Py<PyAny>),
    /// Waiting on an `asyncio.Future` — attach done callback.
    WaitingOnAsyncioFuture(Py<PyAny>),
    /// Unknown awaitable — fall back to `asyncio.ensure_future`.
    FallbackToAsyncio(Py<PyAny>),
}

// ---------------------------------------------------------------------------
// drive_task — the main drive loop
// ---------------------------------------------------------------------------

/// Drive a [`SchedulerTask`] until it suspends or completes.
///
/// This is the hot loop. When the yielded object is `None` (most common case),
/// it immediately loops without suspending. Sub-coroutines are pushed onto the
/// task's coroutine stack and driven inline.
#[allow(
    dead_code,
    clippy::needless_continue,
    reason = "dead_code: will be used by scheduler integration in a future phase; \
              needless_continue: explicit `continue` documents intent to re-enter the drive loop"
)]
pub fn drive_task(py: Python<'_>, task: &mut SchedulerTask, types: &CachedTypes) -> DriveResult {
    loop {
        // Obtain step result, dropping the `coro` borrow before we mutate `task`.
        let step_result = {
            let coro = match task.active_coro(py) {
                Ok(c) => c,
                Err(e) => return DriveResult::Error(e),
            };
            if let Some(err) = task.take_throw_error() {
                step_throw(py, &coro, err)
            } else {
                let send_val = task.take_send_value(py);
                step(py, &coro, send_val.as_ref())
            }
        };
        // `coro` is dropped — safe to mutate `task` below.

        match step_result {
            StepResult::Yielded(obj) => {
                let kind = classify(py, obj.bind(py), types);
                match kind {
                    AwaitableKind::YieldNone => continue, // immediate reschedule
                    AwaitableKind::RustFuture => {
                        return DriveResult::WaitingOnRustFuture(obj);
                    }
                    AwaitableKind::RustEventWaiter => {
                        return DriveResult::WaitingOnEvent(obj);
                    }
                    AwaitableKind::Coroutine => {
                        task.push_coro(obj);
                        continue; // drive the sub-coroutine
                    }
                    AwaitableKind::AsyncioFuture => {
                        return DriveResult::WaitingOnAsyncioFuture(obj);
                    }
                    AwaitableKind::Unknown => {
                        return DriveResult::FallbackToAsyncio(obj);
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
                return DriveResult::Completed(value);
            }
            StepResult::Error(e) => {
                if task.pop_coro() {
                    // Sub-coroutine raised — throw into parent.
                    task.set_throw_error(e);
                    continue;
                }
                return DriveResult::Error(e);
            }
        }
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
    use super::*;

    #[test]
    fn cached_types_resolve() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            // Verify each type object is non-null and callable.
            assert!(!types.asyncio_future.bind(py).is_none());
            assert!(!types.asyncio_task.bind(py).is_none());
            assert!(!types.coroutine_type.bind(py).is_none());
            assert!(!types.generator_type.bind(py).is_none());
            assert!(!types.rust_future_type.bind(py).is_none());
            assert!(!types.rust_event_waiter_type.bind(py).is_none());
        });
    }

    #[test]
    fn classify_none_is_yield_none() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            let none = py.None().into_bound(py);
            assert!(matches!(
                classify(py, &none, &types),
                AwaitableKind::YieldNone
            ));
        });
    }

    #[test]
    fn classify_rust_future() {
        crate::with_py(|py| {
            let types = CachedTypes::resolve(py).unwrap();
            let future = RustFuture::resolved(py.None());
            let py_future = Py::new(py, future).unwrap();
            let bound = py_future.into_bound(py).into_any();
            assert!(matches!(
                classify(py, &bound, &types),
                AwaitableKind::RustFuture
            ));
        });
    }

    #[test]
    fn step_completed_coroutine() {
        crate::with_py(|py| {
            // Create a coroutine that immediately returns 42.
            py.run(
                c"
async def coro():
    return 42
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"coro()", None, None).unwrap();
            let result = step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let num: i64 = val.extract(py).unwrap();
                assert_eq!(num, 42);
            }
        });
    }

    #[test]
    fn step_throw_propagates() {
        crate::with_py(|py| {
            py.run(
                c"
async def coro():
    return 42
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"coro()", None, None).unwrap();
            let err = pyo3::exceptions::PyValueError::new_err("test error");
            let result = step_throw(py, &coro, err);
            assert!(matches!(result, StepResult::Error(_)));
        });
    }
}
