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

    // Primitives
    m.add_class::<crate::scheduler::primitives::Event>()?;
    m.add_class::<crate::scheduler::primitives::EventWaiter>()?;
    m.add_class::<crate::scheduler::primitives::Future>()?;

    // Telemetry
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

    // Bench trace + scheduler stats
    m.add_function(pyo3::wrap_pyfunction!(bench_trace_dump, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(bench_trace_reset, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(scheduler_stats_json, m)?)?;

    Ok(())
}

#[pyfunction]
fn bench_trace_dump() -> Option<String> {
    crate::asgi::bench_trace::read()
}

#[pyfunction]
fn bench_trace_reset() {
    crate::asgi::bench_trace::reset();
}

#[pyfunction]
fn scheduler_stats_json() -> Option<String> {
    let counters = crate::scheduler::counters::get()?;
    let snapshot = counters.snapshot();
    serde_json::to_string(&snapshot).ok()
}
