//! Thread-local asyncio event loop for `spawn_blocking` dispatch.
//!
//! Each tokio blocking pool thread gets its own lightweight asyncio event loop,
//! cached in a `thread_local!`. This allows `run_until_complete(coro)` without
//! hopping to the persistent event loop thread, eliminating
//! `call_soon_threadsafe` overhead and a full thread round-trip per request.

use pyo3::prelude::*;
use std::cell::RefCell;

thread_local! {
    static LOCAL_LOOP: RefCell<Option<Py<PyAny>>> = const { RefCell::new(None) };
}

/// Run a closure with a thread-local asyncio event loop.
///
/// Creates and caches a new `asyncio.new_event_loop()` on first call per thread.
/// Subsequent calls on the same thread reuse the cached loop.
///
/// # Errors
///
/// Returns a `PyErr` if event loop creation or the closure itself fails.
pub fn with_local_loop<F, R>(py: Python<'_>, f: F) -> PyResult<R>
where
    F: FnOnce(Python<'_>, &Bound<'_, PyAny>) -> PyResult<R>,
{
    LOCAL_LOOP.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let asyncio = py.import(c"asyncio")?;
            let loop_obj = asyncio.call_method0(c"new_event_loop")?;
            // Set as current so `asyncio.get_running_loop()` works inside
            // `run_until_complete` — required by FastAPI internals.
            asyncio.call_method1(c"set_event_loop", (&loop_obj,))?;
            tracing::debug!("created thread-local asyncio event loop");
            *slot = Some(loop_obj.unbind());
        }
        // Safety: slot was just assigned `Some(...)` above; this branch is unreachable.
        let Some(loop_ref) = slot.as_ref() else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "thread-local event loop not initialized",
            ));
        };
        f(py, loop_ref.bind(py))
    })
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn thread_local_loop_creation() {
        crate::with_py(|_py| {});

        // Run on a dedicated thread to get a fresh thread-local.
        let result = std::thread::spawn(|| {
            Python::attach(|py| {
                with_local_loop(py, |py, loop_obj| {
                    // Verify it's a running-capable event loop.
                    let is_closed: bool = loop_obj
                        .call_method0(c"is_closed")
                        .unwrap()
                        .extract()
                        .unwrap();
                    assert!(!is_closed);

                    // Drive a trivial coroutine.
                    let code =
                        std::ffi::CString::new("async def _t(): return 99\ncoro = _t()\n").unwrap();
                    let locals = pyo3::types::PyDict::new(py);
                    py.run(&code, None, Some(&locals)).unwrap();
                    let coro = locals.get_item("coro").unwrap().unwrap();
                    let result = loop_obj
                        .call_method1(c"run_until_complete", (&coro,))
                        .unwrap();
                    let val: i64 = result.extract().unwrap();
                    assert_eq!(val, 99);
                    Ok(())
                })
            })
        })
        .join()
        .unwrap();

        result.unwrap();
    }

    #[test]
    fn thread_local_loop_reuse() {
        crate::with_py(|_py| {});

        std::thread::spawn(|| {
            Python::attach(|py| {
                // First call creates the loop.
                let id1 = with_local_loop(py, |_py, loop_obj| {
                    Ok(loop_obj.as_any().getattr(c"__class__")?.str()?.to_string())
                })
                .unwrap();

                // Second call reuses it — no "created thread-local" log expected.
                let id2 = with_local_loop(py, |_py, loop_obj| {
                    Ok(loop_obj.as_any().getattr(c"__class__")?.str()?.to_string())
                })
                .unwrap();

                assert_eq!(id1, id2);
            });
        })
        .join()
        .unwrap();
    }
}
