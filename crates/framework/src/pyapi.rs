//! Python-visible types exported via PyO3.
//!
//! ASGI primitives are registered into the `apx._core` extension module.
//! Users raise `fastapi.HTTPException` directly for HTTP error responses.

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Register framework types into the `apx._core` extension module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // ASGI lifespan protocol types
    m.add_class::<crate::asgi::lifespan::LifespanReceive>()?;
    m.add_class::<crate::asgi::lifespan::LifespanSend>()?;

    // HTTP protocol types
    m.add_class::<crate::protocol::connection::ProtocolFactory>()?;
    m.add_class::<crate::protocol::connection::RustProtocol>()?;
    m.add_class::<crate::protocol::connection::HttpReceive>()?;
    m.add_class::<crate::protocol::router::RustRouter>()?;
    m.add_class::<crate::protocol::writer::RustResponseWriter>()?;

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
    m.add_class::<crate::telemetry::metrics::PyMetricDefinition>()?;
    m.add_function(pyo3::wrap_pyfunction!(
        crate::telemetry::metrics::metric_catalog,
        m
    )?)?;

    Ok(())
}
