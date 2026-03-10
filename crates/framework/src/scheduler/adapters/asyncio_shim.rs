//! Asyncio shim -- monkeypatches `asyncio.sleep` and `asyncio.Event` to
//! delegate to Rust scheduler primitives.
//!
//! Follows the same monkeypatch approach as `experiments/scheduler_scope/probe.py`
//! but replaces logging wrappers with actual Rust-backed implementations.
//!
//! # Patched functions
//!
//! | Original              | Replacement            |
//! |-----------------------|------------------------|
//! | `asyncio.sleep(d)`    | [`shim_sleep`] (returns [`Timer`]) |
//! | `asyncio.Event()`     | [`shim_event`] (returns [`RustEvent`]) |
//!
//! Loop-level method patching is intentionally deferred to the full scheduler
//! integration phase -- only module-level functions are shimmed here.

use pyo3::prelude::*;

use super::super::primitives::{RustEvent, Timer};

// ---------------------------------------------------------------------------
// Replacement functions
// ---------------------------------------------------------------------------

/// Replacement for `asyncio.sleep(delay, result=None)`.
///
/// Returns a [`Timer`] awaitable that fires after `delay` seconds.
/// The `result` parameter is accepted for API compatibility with
/// `asyncio.sleep` but is currently ignored (returns `None` on completion).
#[pyfunction]
#[pyo3(signature = (delay, result=None))]
fn shim_sleep(py: Python<'_>, delay: f64, result: Option<Py<PyAny>>) -> PyResult<Timer> {
    let _ = result; // TODO: support the result parameter
    Timer::new(py, delay)
}

/// Replacement for `asyncio.Event()`.
///
/// Returns a [`RustEvent`] backed by `tokio::sync::Notify` instead of the
/// stock asyncio event implementation.
#[pyfunction]
fn shim_event() -> RustEvent {
    RustEvent::new()
}

// ---------------------------------------------------------------------------
// AsyncioShim -- install / uninstall bookkeeping
// ---------------------------------------------------------------------------

/// Saved original: `(module, attribute_name, original_value)`.
type SavedOriginal = (Py<PyAny>, String, Py<PyAny>);

/// Manages the lifecycle of asyncio module-level monkeypatches.
///
/// Saves the original functions on [`install`](AsyncioShim::install) and
/// restores them on [`uninstall`](AsyncioShim::uninstall).
#[pyclass(module = "apx._core")]
pub struct AsyncioShim {
    /// Saved originals for restoration on uninstall.
    originals: Vec<SavedOriginal>,
    /// Whether the shim is currently active.
    installed: bool,
}

#[pymethods]
impl AsyncioShim {
    #[new]
    fn new() -> Self {
        Self {
            originals: Vec::new(),
            installed: false,
        }
    }

    /// Check whether the shim is currently installed.
    fn is_installed(&self) -> bool {
        self.installed
    }
}

impl AsyncioShim {
    /// Install asyncio shims that delegate to Rust scheduler primitives.
    ///
    /// Patches `asyncio.sleep` and `asyncio.Event` at the module level.
    /// The original functions are saved so they can be restored via
    /// [`uninstall`](AsyncioShim::uninstall).
    pub fn install(py: Python<'_>) -> PyResult<Self> {
        let asyncio = py.import(c"asyncio")?;
        let mut originals: Vec<SavedOriginal> = Vec::new();

        // -- asyncio.sleep -> shim_sleep ------------------------------------
        let orig_sleep = asyncio.getattr(c"sleep")?.unbind();
        originals.push((
            asyncio.as_any().clone().unbind(),
            "sleep".to_owned(),
            orig_sleep,
        ));
        let replacement_sleep = wrap_pyfunction!(shim_sleep, py)?;
        asyncio.setattr(c"sleep", replacement_sleep)?;

        // -- asyncio.Event -> shim_event ------------------------------------
        let orig_event = asyncio.getattr(c"Event")?.unbind();
        originals.push((
            asyncio.as_any().clone().unbind(),
            "Event".to_owned(),
            orig_event,
        ));
        let replacement_event = wrap_pyfunction!(shim_event, py)?;
        asyncio.setattr(c"Event", replacement_event)?;

        Ok(Self {
            originals,
            installed: true,
        })
    }

    /// Uninstall shims and restore the original asyncio functions.
    ///
    /// Safe to call multiple times -- subsequent calls are no-ops.
    pub fn uninstall(&mut self, py: Python<'_>) -> PyResult<()> {
        if !self.installed {
            return Ok(());
        }
        for (module, name, original) in self.originals.drain(..) {
            module.bind(py).setattr(&*name, original.bind(py))?;
        }
        self.installed = false;
        Ok(())
    }
}

impl std::fmt::Debug for AsyncioShim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncioShim")
            .field("installed", &self.installed)
            .field("patched_count", &self.originals.len())
            .finish()
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
    fn shim_sleep_returns_timer() {
        crate::with_py(|py| {
            let timer = shim_sleep(py, 1.0, None).unwrap();
            assert!(!timer.done(py));
        });
    }

    #[test]
    fn shim_sleep_zero_delay() {
        crate::with_py(|py| {
            let timer = shim_sleep(py, 0.0, None).unwrap();
            // Zero-delay timer wraps a resolved RustFuture.
            assert!(timer.done(py));
        });
    }

    #[test]
    fn shim_event_returns_unset_event() {
        let event = shim_event();
        assert!(!event.is_set());
    }

    #[test]
    fn install_and_uninstall_round_trip() {
        crate::with_py(|py| {
            // Capture the original asyncio.sleep identity.
            let asyncio = py.import(c"asyncio").unwrap();
            let orig_sleep = asyncio.getattr(c"sleep").unwrap().unbind();

            // Install shims.
            let mut shim = AsyncioShim::install(py).unwrap();
            assert!(shim.is_installed());

            // asyncio.sleep should now be our replacement.
            let patched_sleep = asyncio.getattr(c"sleep").unwrap();
            assert!(!patched_sleep.is(orig_sleep.bind(py)));

            // Uninstall.
            shim.uninstall(py).unwrap();
            assert!(!shim.is_installed());

            // asyncio.sleep should be restored to the original.
            let restored_sleep = asyncio.getattr(c"sleep").unwrap();
            assert!(restored_sleep.is(orig_sleep.bind(py)));
        });
    }

    #[test]
    fn uninstall_idempotent() {
        crate::with_py(|py| {
            let mut shim = AsyncioShim::install(py).unwrap();
            shim.uninstall(py).unwrap();
            // Second uninstall should be a no-op.
            shim.uninstall(py).unwrap();
            assert!(!shim.is_installed());
        });
    }

    #[test]
    fn debug_format() {
        let shim = AsyncioShim::new();
        let dbg = format!("{shim:?}");
        assert!(dbg.contains("AsyncioShim"));
        assert!(dbg.contains("installed: false"));
    }
}
