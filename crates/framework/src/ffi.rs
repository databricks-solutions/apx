//! CPython C-API operations behind safe Rust interfaces.
//!
//! All `unsafe` FFI code in the framework crate is concentrated here.
//! Callers interact through the [`CoroutineOps`] trait (for coroutine
//! stepping/classification) or the standalone utility functions
//! [`copy_context`] and [`new_presized_dict`].

use pyo3::PyTypeInfo;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyType};

use crate::scheduler::primitives::{EventWaiter, Future};

// ---------------------------------------------------------------------------
// StepResult — outcome of advancing a coroutine one step
// ---------------------------------------------------------------------------

/// Outcome of a single `send` / `throw` cycle on a coroutine.
#[derive(Debug)]
pub enum StepResult {
    /// Coroutine yielded a value (needs classification).
    Yielded(Py<PyAny>),
    /// Coroutine completed (raised `StopIteration` with a value).
    Completed(Py<PyAny>),
    /// Coroutine raised an exception.
    Error(PyErr),
}

// ---------------------------------------------------------------------------
// AwaitableKind — classification of yielded objects
// ---------------------------------------------------------------------------

/// Classification of a yielded object from a coroutine.
#[derive(Debug, Clone, Copy)]
pub enum AwaitableKind {
    /// Our own `Future` — poll directly via `__next__`.
    Future,
    /// Our own `EventWaiter` — check event flag.
    EventWaiter,
    /// `asyncio.Future` (not Task) — attach done callback.
    AsyncioFuture,
    /// A coroutine object — push onto coroutine stack.
    Coroutine,
    /// `yield None` — reschedule immediately (like `call_soon`).
    YieldNone,
    /// Unknown awaitable — unsupported, returns error.
    Unknown,
}

// ---------------------------------------------------------------------------
// CoroutineOps trait
// ---------------------------------------------------------------------------

/// Operations for stepping and classifying Python coroutines.
///
/// The trait boundary separates *what* the driver needs (step, classify)
/// from *how* it's done (raw FFI vs safe PyO3). Implementations:
/// - [`FfiCoroutineOps`]: direct CPython C-API calls (current, fast)
/// - Future: safe PyO3 wrappers (for debugging / free-threaded Python)
pub trait CoroutineOps: Send + Sync + std::fmt::Debug {
    /// Advance a coroutine one step by sending a value.
    fn step(&self, py: Python<'_>, coro: &Py<PyAny>, value: Option<&Py<PyAny>>) -> StepResult;

    /// Classify a yielded value to determine what the driver should do next.
    fn classify(&self, py: Python<'_>, yielded: &Py<PyAny>) -> AwaitableKind;

    /// Advance a coroutine one step by throwing an exception.
    ///
    /// Default implementation using safe PyO3 `coro.throw()`.
    /// Override only if the implementation needs different semantics.
    fn step_throw(&self, py: Python<'_>, coro: &Py<PyAny>, err: PyErr) -> StepResult {
        match coro.call_method1(py, c"throw", (err.value(py),)) {
            Ok(yielded) => StepResult::Yielded(yielded),
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
}

// ---------------------------------------------------------------------------
// FfiCoroutineOps — unsafe CPython C-API implementation
// ---------------------------------------------------------------------------

/// Direct CPython C-API implementation of [`CoroutineOps`].
///
/// Caches raw type pointers and interned attribute names at startup for
/// hot-path `ob_type` comparisons and attribute lookups.
pub struct FfiCoroutineOps {
    /// `types.CoroutineType` — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    coroutine_type: Py<PyType>,
    /// Our `Future` type object — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    future_type: Py<PyType>,
    /// Our `EventWaiter` type object — retained for pointer extraction.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "pointer extracted; Py<PyType> keeps type alive")
    )]
    event_waiter_type: Py<PyType>,
    // -- Raw type pointers for hot-path ob_type comparison ------------------
    //
    // Borrowed from the Py<PyType> fields above — valid as long as
    // FfiCoroutineOps lives (the Py handles prevent deallocation).
    /// Raw `ob_type` pointer for our `Future` pyclass.
    future_type_ptr: *mut pyo3::ffi::PyObject,
    /// Raw `ob_type` pointer for our `EventWaiter` pyclass.
    event_waiter_type_ptr: *mut pyo3::ffi::PyObject,
    /// Raw `ob_type` pointer for `types.CoroutineType`.
    coroutine_type_ptr: *mut pyo3::ffi::PyObject,
    /// Interned `"_asyncio_future_blocking"` attribute name for duck-type
    /// asyncio.Future detection via `_asyncio_future_blocking` attribute.
    asyncio_future_blocking_attr: Py<PyAny>,
    /// Cached `Py_None` pointer for yield-None fast path.
    py_none_ptr: *mut pyo3::ffi::PyObject,
}

// Safety: The raw pointers in FfiCoroutineOps are borrowed from Py<PyType>
// handles that are kept alive for the struct's entire lifetime. The pointers
// are only dereferenced under the GIL (which Python<'_> proves), so they are
// safe to send across threads.
#[expect(
    unsafe_code,
    reason = "raw pointers borrowed from GIL-protected Py handles"
)]
unsafe impl Send for FfiCoroutineOps {}
#[expect(
    unsafe_code,
    reason = "raw pointers borrowed from GIL-protected Py handles"
)]
unsafe impl Sync for FfiCoroutineOps {}

impl std::fmt::Debug for FfiCoroutineOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiCoroutineOps").finish_non_exhaustive()
    }
}

impl FfiCoroutineOps {
    /// Resolve all Python types once at startup.
    pub fn resolve(py: Python<'_>) -> PyResult<Self> {
        let types = py.import(c"types")?;

        let future_type = Future::type_object(py).unbind();
        let event_waiter_type = EventWaiter::type_object(py).unbind();
        let coroutine_type = types
            .getattr(c"CoroutineType")?
            .cast_into::<PyType>()?
            .unbind();

        // Cache raw type pointers for hot-path ob_type comparison.
        let future_type_ptr = future_type.as_ptr();
        let event_waiter_type_ptr = event_waiter_type.as_ptr();
        let coroutine_type_ptr = coroutine_type.as_ptr();
        let py_none_ptr = py.None().as_ptr();

        // Intern "_asyncio_future_blocking" for duck-type asyncio.Future detection.
        let asyncio_future_blocking_attr: Py<PyAny> = pyo3::intern!(py, "_asyncio_future_blocking")
            .clone()
            .unbind()
            .into();

        Ok(Self {
            coroutine_type,
            future_type,
            event_waiter_type,
            future_type_ptr,
            event_waiter_type_ptr,
            coroutine_type_ptr,
            asyncio_future_blocking_attr,
            py_none_ptr,
        })
    }
}

/// Handle `PYGEN_ERROR` from `PyIter_Send`.
///
/// Guards against edge cases where no exception is set (e.g. generator
/// already exhausted). Treats `StopIteration` as normal completion.
#[expect(unsafe_code, reason = "FFI calls to PyErr_Occurred / PyErr_Clear")]
fn handle_pygen_error(py: Python<'_>) -> StepResult {
    if unsafe { pyo3::ffi::PyErr_Occurred().is_null() } {
        return StepResult::Completed(py.None());
    }
    let err = PyErr::fetch(py);
    if err.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
        let value = err
            .value(py)
            .getattr(c"value")
            .map_or_else(|_| py.None(), |v| v.unbind());
        StepResult::Completed(value)
    } else {
        StepResult::Error(err)
    }
}

impl CoroutineOps for FfiCoroutineOps {
    /// Advance a coroutine one step via C-level `PyIter_Send`.
    ///
    /// # Safety invariants (upheld by callers)
    /// - `coro` is a valid Python coroutine/iterator
    /// - `py` proves the GIL is held
    ///
    /// `PyIter_Send` handles `StopIteration` internally — no Python exception
    /// on `PYGEN_RETURN`.
    #[expect(
        unsafe_code,
        reason = "FFI call to PyIter_Send for hot-path performance"
    )]
    fn step(&self, py: Python<'_>, coro: &Py<PyAny>, value: Option<&Py<PyAny>>) -> StepResult {
        let send_arg = value.map_or(std::ptr::null_mut(), |v| v.as_ptr());
        let mut result_ptr: *mut pyo3::ffi::PyObject = std::ptr::null_mut();

        // Safety: coro is a valid Python coroutine, py proves GIL is held.
        // PyIter_Send handles StopIteration internally — no Python exception on PYGEN_RETURN.
        let status =
            unsafe { pyo3::ffi::PyIter_Send(coro.as_ptr(), send_arg, &raw mut result_ptr) };

        match status {
            pyo3::ffi::PySendResult::PYGEN_NEXT => {
                // Safety: PYGEN_NEXT guarantees result_ptr is a new reference.
                let obj = unsafe { Bound::from_owned_ptr(py, result_ptr) }.unbind();
                StepResult::Yielded(obj)
            }
            pyo3::ffi::PySendResult::PYGEN_RETURN => {
                let value = if result_ptr.is_null() {
                    py.None()
                } else {
                    // Safety: PYGEN_RETURN with non-null is a new reference to the return value.
                    unsafe { Bound::from_owned_ptr(py, result_ptr) }.unbind()
                };
                StepResult::Completed(value)
            }
            pyo3::ffi::PySendResult::PYGEN_ERROR => handle_pygen_error(py),
        }
    }

    /// Classify a yielded value to determine what the driver should do next.
    ///
    /// Check order is optimised for the common case: `yield None` first (36% of
    /// all calls), then our own types, then coroutines, then asyncio futures.
    ///
    /// Uses direct `ob_type` pointer comparisons for our own pyclass types (no
    /// subclassing) and `Py_None` singleton comparison. Falls back to duck-type
    /// attribute check for asyncio futures (`_asyncio_future_blocking`).
    #[expect(
        unsafe_code,
        reason = "ob_type pointer read under GIL for hot-path classification"
    )]
    fn classify(&self, _py: Python<'_>, yielded: &Py<PyAny>) -> AwaitableKind {
        let obj_ptr = yielded.as_ptr();

        // Fast path: yield None (36% of all calls) — pointer comparison against singleton.
        if obj_ptr == self.py_none_ptr {
            return AwaitableKind::YieldNone;
        }

        // Safety: ob_type is always valid for a live Python object while GIL is held.
        let ob_type = unsafe { (*obj_ptr).ob_type.cast::<pyo3::ffi::PyObject>() };

        // Exact type match for our own pyclass types (no subclassing allowed).
        if ob_type == self.future_type_ptr {
            return AwaitableKind::Future;
        }
        if ob_type == self.event_waiter_type_ptr {
            return AwaitableKind::EventWaiter;
        }

        // Coroutine: exact type match (native coroutines have a fixed type).
        if ob_type == self.coroutine_type_ptr {
            return AwaitableKind::Coroutine;
        }

        // asyncio.Future detection via _asyncio_future_blocking attribute.
        // Duck-type asyncio.Future detection — checking for the
        // attribute is faster than isinstance on the full MRO.
        let blocking_attr = unsafe {
            pyo3::ffi::PyObject_GetAttr(obj_ptr, self.asyncio_future_blocking_attr.as_ptr())
        };
        if !blocking_attr.is_null() {
            unsafe { pyo3::ffi::Py_DECREF(blocking_attr) };
            return AwaitableKind::AsyncioFuture;
        }
        // Clear the AttributeError from the failed GetAttr.
        unsafe { pyo3::ffi::PyErr_Clear() };

        AwaitableKind::Unknown
    }
}

// ---------------------------------------------------------------------------
// copy_context — contextvars snapshot
// ---------------------------------------------------------------------------

/// Copy the current contextvars context via C-level `PyContext_CopyCurrent`.
///
/// Returns `None` if the copy fails (should not happen in practice).
#[expect(unsafe_code, reason = "FFI call to PyContext_CopyCurrent")]
pub fn copy_context(py: Python<'_>) -> Option<Py<PyAny>> {
    let ctx_ptr = unsafe { pyo3::ffi::PyContext_CopyCurrent() };
    if ctx_ptr.is_null() {
        // Clear any pending exception and return None.
        unsafe { pyo3::ffi::PyErr_Clear() };
        None
    } else {
        // Safety: PyContext_CopyCurrent returns a new reference on success.
        Some(unsafe { Bound::from_owned_ptr(py, ctx_ptr) }.unbind())
    }
}

// ---------------------------------------------------------------------------
// new_presized_dict — pre-allocated dict
// ---------------------------------------------------------------------------

// CPython internal: create a dict pre-sized for `minused` keys.
// Stable across CPython 3.8-3.13. Not exposed by pyo3-ffi (marked private),
// so we declare it manually.
#[expect(unsafe_code, reason = "CPython FFI declaration for dict pre-sizing")]
unsafe extern "C" {
    fn _PyDict_NewPresized(minused: pyo3::ffi::Py_ssize_t) -> *mut pyo3::ffi::PyObject;
}

/// Create a `PyDict` with pre-allocated capacity.
///
/// Avoids internal rehashing for dicts with a known number of keys.
#[expect(unsafe_code, reason = "CPython FFI for dict pre-sizing")]
pub fn new_presized_dict(py: Python<'_>, capacity: isize) -> Bound<'_, PyDict> {
    let ptr = unsafe { _PyDict_NewPresized(capacity) };
    if ptr.is_null() {
        return PyDict::new(py);
    }
    unsafe { Bound::from_owned_ptr(py, ptr).cast_into_unchecked() }
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
    fn ffi_coroutine_ops_resolve() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            // Verify each type object is non-null and callable.
            assert!(!ops.coroutine_type.bind(py).is_none());
            assert!(!ops.future_type.bind(py).is_none());
            assert!(!ops.event_waiter_type.bind(py).is_none());
        });
    }

    #[test]
    fn classify_none_is_yield_none() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            let none = py.None();
            assert!(matches!(ops.classify(py, &none), AwaitableKind::YieldNone));
        });
    }

    #[test]
    fn classify_rust_future() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            let future = Future::resolved(py.None());
            let py_future = Py::new(py, future).unwrap().into_any();
            assert!(matches!(
                ops.classify(py, &py_future),
                AwaitableKind::Future
            ));
        });
    }

    #[test]
    fn classify_coroutine() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            py.run(c"async def _c(): pass", None, None).unwrap();
            let coro = py.eval(c"_c()", None, None).unwrap().unbind();
            assert!(matches!(ops.classify(py, &coro), AwaitableKind::Coroutine));
        });
    }

    #[test]
    fn classify_asyncio_future() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            // Create a real asyncio.Future (requires a running loop).
            let asyncio = py.import(c"asyncio").unwrap();
            let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
            let fut = loop_obj.call_method0(c"create_future").unwrap();
            let fut_py = fut.unbind();
            assert!(
                matches!(ops.classify(py, &fut_py), AwaitableKind::AsyncioFuture),
                "asyncio.Future should be classified as AsyncioFuture"
            );
            let _ = loop_obj.call_method0(c"close");
        });
    }

    #[test]
    fn step_completed_coroutine() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
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
            let coro = py.eval(c"coro()", None, None).unwrap().unbind();
            let result = ops.step(py, &coro, None);
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
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            py.run(
                c"
async def coro():
    return 42
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"coro()", None, None).unwrap().unbind();
            let err = pyo3::exceptions::PyValueError::new_err("test error");
            let result = ops.step_throw(py, &coro, err);
            assert!(matches!(result, StepResult::Error(_)));
        });
    }

    #[test]
    fn step_yielding_coroutine() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            // Coroutine that yields None once, then returns "done".
            py.run(
                c"
import asyncio
async def yielding():
    await asyncio.sleep(0)  # yields None
    return 'done'
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"yielding()", None, None).unwrap().unbind();
            // First step: should yield (asyncio.sleep yields a future, but
            // the inner coroutine yields None).
            let result = ops.step(py, &coro, None);
            assert!(
                matches!(result, StepResult::Yielded(_)),
                "expected Yielded, got {result:?}",
            );
            // Second step: send None back, should complete.
            let result = ops.step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let s: String = val.extract(py).unwrap();
                assert_eq!(s, "done");
            }
        });
    }

    #[test]
    fn step_sub_coroutine() {
        crate::with_py(|py| {
            let ops = FfiCoroutineOps::resolve(py).unwrap();
            // Inner coroutine yields (via asyncio.sleep(0)), then returns.
            // outer awaits inner, so the first step yields through the chain.
            py.run(
                c"
import asyncio
async def inner():
    await asyncio.sleep(0)
    return 99

async def outer():
    val = await inner()
    return val + 1
",
                None,
                None,
            )
            .unwrap();
            let coro = py.eval(c"outer()", None, None).unwrap().unbind();
            // First step: inner yields (via sleep(0)), propagated to outer.
            let result = ops.step(py, &coro, None);
            assert!(
                matches!(result, StepResult::Yielded(_)),
                "expected Yielded, got {result:?}",
            );
            // Second step: send None back, inner completes, outer completes.
            let result = ops.step(py, &coro, None);
            assert!(matches!(result, StepResult::Completed(_)));
            if let StepResult::Completed(val) = result {
                let num: i64 = val.extract(py).unwrap();
                assert_eq!(num, 100);
            }
        });
    }
}
