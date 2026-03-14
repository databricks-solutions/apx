//! Thread-local asyncio event loop for `spawn_blocking` dispatch.
//!
//! Each tokio blocking pool thread gets its own lightweight asyncio event loop,
//! cached in a `thread_local!`. This allows `run_until_complete(coro)` without
//! hopping to the persistent event loop thread, eliminating
//! `call_soon_threadsafe` overhead and a full thread round-trip per request.
#![allow(dead_code)] // set_running_loop / RunningLoopGuard wired in try-sync-first inline dispatch.

use pyo3::prelude::*;
use std::cell::RefCell;
use std::sync::Once;

thread_local! {
    static LOCAL_LOOP: RefCell<Option<Py<PyAny>>> = const { RefCell::new(None) };
}

static CURRENT_TASK_PATCH: Once = Once::new();

/// Install a thread-safe `asyncio.current_task` monkeypatch.
///
/// Python 3.11's `asyncio.current_task()` uses a global `_current_tasks[loop]`
/// dict — not per-thread. Under concurrent inline dispatch, multiple blocking
/// threads race on this entry. This patch adds a `threading.local()` check
/// before the dict lookup, making current_task per-thread safe.
///
/// On Python 3.12+, asyncio uses C-level per-thread state, so this patch
/// is a harmless no-op (the thread-local is never set for those versions).
fn install_current_task_patch(py: Python<'_>) {
    CURRENT_TASK_PATCH.call_once(|| {
        let code = c"
import asyncio, threading
_apx_tl = threading.local()
_orig_ct = asyncio.current_task
def _ct(loop=None):
    t = getattr(_apx_tl, 'v', None)
    return t if t is not None else _orig_ct(loop)
asyncio.current_task = _ct
asyncio._apx_tl = _apx_tl
";
        if let Err(e) = py.run(code, None, None) {
            tracing::warn!(error = %e, "failed to install current_task patch");
        }
    });
}

/// Set the per-thread current task proxy for `asyncio.current_task()`.
pub fn set_thread_current_task(py: Python<'_>, proxy: &Py<PyAny>) {
    if let Ok(tl) = py.import(c"asyncio").and_then(|m| m.getattr(c"_apx_tl")) {
        let _ = tl.setattr(c"v", proxy);
    }
}

/// Clear the per-thread current task proxy.
pub fn clear_thread_current_task(py: Python<'_>) {
    if let Ok(tl) = py.import(c"asyncio").and_then(|m| m.getattr(c"_apx_tl")) {
        let _ = tl.setattr(c"v", py.None());
    }
}

/// Run a closure with a thread-local asyncio event loop.
///
/// Creates and caches a new `asyncio.new_event_loop()` on first call per thread.
/// Subsequent calls on the same thread reuse the cached loop.
///
/// # Errors
///
/// Returns a `PyErr` if event loop creation or the closure itself fails.
#[cfg(test)]
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

/// Set the shared event loop as "running" on the current thread.
///
/// Uses `asyncio._set_running_loop(loop_obj)` — thread-local state in CPython.
/// This makes `asyncio.get_running_loop()` succeed, which is required for
/// `TaskProxy` installation (`asyncio.current_task()` support).
///
/// Also sets `sniffio`'s thread-local so that `anyio` detects the asyncio
/// context without relying on `asyncio.current_task()` — which races under
/// concurrent inline dispatch (multiple threads share `_current_tasks[loop]`).
///
/// # Invariants
///
/// - `_set_running_loop` is thread-local in CPython — concurrent blocking
///   threads each get their own "running loop" without interference.
/// - Only `yield None`, `asyncio.Future`, and coroutines are driven inline;
///   suspended handlers resume via the ready queue on the event loop thread.
/// - The loop is not truly running on this thread — `loop.run_until_complete()`
///   would fail with "event loop is already running".
///
/// Must be paired with [`clear_running_loop`] before releasing the GIL.
pub fn set_running_loop(py: Python<'_>, loop_ref: &Py<PyAny>) -> PyResult<()> {
    let asyncio = py.import(c"asyncio")?;
    asyncio.call_method1(c"_set_running_loop", (loop_ref,))?;
    // Tell sniffio/anyio we're in an asyncio context (thread-local, no race).
    if let Ok(tl) = py
        .import(c"sniffio._impl")
        .and_then(|m| m.getattr(c"thread_local"))
    {
        let _ = tl.setattr(c"name", "asyncio");
    }
    // Install thread-safe current_task patch (once, first call only).
    install_current_task_patch(py);
    Ok(())
}

/// Clear the running loop for the current thread.
pub fn clear_running_loop(py: Python<'_>) {
    if let Ok(asyncio) = py.import(c"asyncio") {
        let _ = asyncio.call_method1(c"_set_running_loop", (py.None(),));
    }
    // Clear sniffio thread-local.
    if let Ok(tl) = py
        .import(c"sniffio._impl")
        .and_then(|m| m.getattr(c"thread_local"))
    {
        let _ = tl.delattr(c"name");
    }
}

/// RAII guard that clears the running loop and thread-local task on drop.
///
/// Ensures exception safety: if the inline dispatch panics or returns early,
/// the running loop and current task are always cleared.
pub struct RunningLoopGuard;

impl Drop for RunningLoopGuard {
    fn drop(&mut self) {
        Python::attach(|py| {
            clear_thread_current_task(py);
            clear_running_loop(py);
        });
    }
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

    #[test]
    fn set_and_clear_running_loop() {
        crate::with_py(|_py| {});

        std::thread::spawn(|| {
            Python::attach(|py| {
                let asyncio = py.import(c"asyncio").unwrap();
                let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
                let loop_ref = loop_obj.unbind();

                // Before setting: get_running_loop should fail.
                assert!(asyncio.call_method0(c"get_running_loop").is_err());

                // Set running loop.
                set_running_loop(py, &loop_ref).unwrap();
                let running = asyncio.call_method0(c"get_running_loop").unwrap();
                assert!(running.is(loop_ref.bind(py)));

                // Clear running loop.
                clear_running_loop(py);
                assert!(asyncio.call_method0(c"get_running_loop").is_err());
            });
        })
        .join()
        .unwrap();
    }

    #[test]
    fn running_loop_guard_cleans_up() {
        crate::with_py(|_py| {});

        std::thread::spawn(|| {
            Python::attach(|py| {
                let asyncio = py.import(c"asyncio").unwrap();
                let loop_obj = asyncio.call_method0(c"new_event_loop").unwrap();
                let loop_ref = loop_obj.unbind();

                set_running_loop(py, &loop_ref).unwrap();
                assert!(asyncio.call_method0(c"get_running_loop").is_ok());

                // Drop guard — should clear the running loop.
                {
                    let _guard = RunningLoopGuard;
                }

                assert!(asyncio.call_method0(c"get_running_loop").is_err());
            });
        })
        .join()
        .unwrap();
    }
}
