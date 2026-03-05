//! Python-visible types exported via PyO3.
//!
//! Exception types and ASGI primitives registered into the `apx._core`
//! extension module. Used by the Rust bridge and by Python user code
//! (exception handling).

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

// ── Exceptions ──────────────────────────────────────────────────────────

pyo3::create_exception!(
    apx._core,
    NotFound,
    PyException,
    "Return a 404 Not Found response."
);
pyo3::create_exception!(
    apx._core,
    BadRequest,
    PyException,
    "Return a 400 Bad Request response."
);
pyo3::create_exception!(
    apx._core,
    Forbidden,
    PyException,
    "Return a 403 Forbidden response."
);

// ── Module registration ─────────────────────────────────────────────────

/// Register all framework types and exceptions into the given Python module.
///
/// Called from the top-level `#[pymodule]` in `crates/apx/src/lib.rs`.
///
/// # Errors
///
/// Returns an error if any type or exception registration fails.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<crate::bridge::asgi::AsgiReceive>()?;
    m.add_class::<crate::bridge::asgi::AsgiSend>()?;

    m.add("NotFound", m.py().get_type::<NotFound>())?;
    m.add("BadRequest", m.py().get_type::<BadRequest>())?;
    m.add("Forbidden", m.py().get_type::<Forbidden>())?;

    Ok(())
}
