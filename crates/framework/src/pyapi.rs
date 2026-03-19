//! Python-visible types exported via PyO3.
//!
//! ASGI primitives are registered into the `apx._core` extension module.
//! Users raise `fastapi.HTTPException` directly for HTTP error responses.

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Register framework types into the `apx._core` extension module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<crate::asgi::scope::AsgiReceive>()?;
    m.add_class::<crate::asgi::scope::AsgiSend>()?;

    // Telemetry
    m.add_class::<crate::telemetry::spans::StatusCode>()?;
    m.add_class::<crate::telemetry::spans::SpanHandle>()?;
    m.add_class::<crate::telemetry::metrics::RustCounter>()?;
    m.add_class::<crate::telemetry::metrics::RustHistogram>()?;
    m.add_class::<crate::telemetry::metrics::RustGauge>()?;
    m.add_function(pyo3::wrap_pyfunction!(
        crate::telemetry::metrics::create_counter,
        m
    )?)?;
    m.add_function(pyo3::wrap_pyfunction!(
        crate::telemetry::metrics::create_histogram,
        m
    )?)?;
    m.add_function(pyo3::wrap_pyfunction!(
        crate::telemetry::metrics::create_gauge,
        m
    )?)?;
    m.add_function(pyo3::wrap_pyfunction!(
        crate::telemetry::logging::emit_log,
        m
    )?)?;

    // Request stats
    m.add_function(pyo3::wrap_pyfunction!(request_stats_json, m)?)?;

    Ok(())
}

#[pyfunction]
fn request_stats_json() -> Option<String> {
    let counters = crate::io::counters::get()?;
    let snapshot = counters.snapshot();
    serde_json::to_string(&snapshot).ok()
}
