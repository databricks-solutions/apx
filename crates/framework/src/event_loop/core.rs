//! Asyncio event loop utilities.
//!
//! Provides policy installation, loop creation, scheduler setup, and task
//! cancellation. These are used by [`super::inline::InlineEventLoop`].

use pyo3::prelude::*;

/// Install the event loop policy (uvloop or asyncio) before creating the loop.
///
/// Must be called before `asyncio.new_event_loop()` so the factory picks up
/// the right policy.
pub fn install_loop_policy(py: Python<'_>, policy: &str) {
    if policy == "uvloop" {
        match py.import(c"uvloop") {
            Ok(uvloop) => {
                let Ok(asyncio) = py.import(c"asyncio") else {
                    tracing::error!("failed to import asyncio for uvloop policy install");
                    return;
                };
                let Ok(policy_obj) = uvloop.call_method0(c"EventLoopPolicy") else {
                    tracing::error!("uvloop.EventLoopPolicy() call failed");
                    return;
                };
                if let Err(e) = asyncio.call_method1(c"set_event_loop_policy", (policy_obj,)) {
                    tracing::error!(error = %e, "asyncio.set_event_loop_policy() failed");
                    return;
                }
                tracing::info!("installed uvloop event loop policy");
            }
            Err(e) => {
                tracing::warn!(error = %e, "uvloop not available, falling back to asyncio");
            }
        }
    } else {
        tracing::info!(policy, "using asyncio event loop policy");
    }
}

/// Create an asyncio event loop as the I/O reactor (socket ops, DNS).
///
/// The Rust scheduler drives all coroutine scheduling; asyncio only
/// resolves `asyncio.Future`s from network I/O libraries.
pub fn create_event_loop(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    tracing::info!("creating asyncio I/O reactor");
    py.import(c"asyncio")?.call_method0(c"new_event_loop")
}

/// Initialize Rust scheduler state on the current event loop thread.
///
/// Stores the tokio runtime handle in a thread-local. Does NOT monkeypatch
/// asyncio — native asyncio coroutines are handled by the driver's
/// `WaitingOnAsyncioFuture` path.
pub fn install_rust_scheduler(
    _py: Python<'_>,
    tokio_handle: Option<tokio::runtime::Handle>,
) -> PyResult<()> {
    if let Some(handle) = tokio_handle {
        crate::scheduler::set_tokio_handle(handle);
    }
    tracing::info!("rust scheduler initialized (no asyncio monkeypatching)");
    Ok(())
}

/// Cancel all pending asyncio tasks and run them to completion.
///
/// Without this step, `loop.close()` leaves live tasks whose cleanup
/// callbacks call `call_soon_threadsafe` on the already-closed loop,
/// producing `RuntimeError: Event loop is closed` on stderr.
pub fn cancel_pending_tasks(py: Python<'_>, event_loop: &Bound<'_, PyAny>) {
    let Ok(asyncio) = py.import(c"asyncio") else {
        return;
    };
    let Ok(tasks) = asyncio.call_method1(c"all_tasks", (event_loop,)) else {
        return;
    };
    let Ok(task_iter) = tasks.try_iter() else {
        return;
    };
    for task in task_iter.flatten() {
        let _ = task.call_method0(c"cancel");
    }
    // Drive cancelled tasks so their CancelledError propagates.
    let Ok(gather) = asyncio.call_method(c"gather", (&tasks,), Some(&gather_kwargs(py))) else {
        return;
    };
    let _ = event_loop.call_method1(c"run_until_complete", (gather,));
}

/// Build `return_exceptions=True` kwargs for `asyncio.gather`.
fn gather_kwargs(py: Python<'_>) -> Bound<'_, pyo3::types::PyDict> {
    let kwargs = pyo3::types::PyDict::new(py);
    let _ = kwargs.set_item("return_exceptions", true);
    kwargs
}
