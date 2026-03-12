//! OTLP metrics instruments exposed to Python via PyO3.
//!
//! Thin wrappers around `opentelemetry::metrics` instruments.
//! When OTEL is disabled, the global meter returns noop instruments
//! — zero overhead automatically.

use opentelemetry::metrics::{Meter, MeterProvider};
use pyo3::prelude::*;
use std::collections::HashMap;

/// Obtain the user-facing meter (backed by configured provider or global noop).
fn user_meter() -> Meter {
    apx_core::tracing_init::meter_provider().map_or_else(
        || opentelemetry::global::meter("apx.user"),
        |mp| mp.meter("apx.user"),
    )
}

// ── Counter ─────────────────────────────────────────────────────────────

/// An OTLP counter metric.
#[pyclass(module = "apx._core")]
pub struct RustCounter {
    inner: opentelemetry::metrics::Counter<u64>,
}

impl std::fmt::Debug for RustCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustCounter").finish_non_exhaustive()
    }
}

#[pymethods]
impl RustCounter {
    /// Increment the counter.
    #[pyo3(signature = (value=1, labels=None))]
    fn inc(&self, value: u64, labels: Option<HashMap<String, String>>) {
        let attrs = labels_to_kv(labels);
        self.inner.add(value, &attrs);
    }
}

// ── Histogram ───────────────────────────────────────────────────────────

/// An OTLP histogram metric.
#[pyclass(module = "apx._core")]
pub struct RustHistogram {
    inner: opentelemetry::metrics::Histogram<f64>,
}

impl std::fmt::Debug for RustHistogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustHistogram").finish_non_exhaustive()
    }
}

#[pymethods]
impl RustHistogram {
    /// Record an observation.
    #[pyo3(signature = (value, labels=None))]
    fn observe(&self, value: f64, labels: Option<HashMap<String, String>>) {
        let attrs = labels_to_kv(labels);
        self.inner.record(value, &attrs);
    }
}

// ── Gauge ───────────────────────────────────────────────────────────────

/// An OTLP gauge metric.
#[pyclass(module = "apx._core")]
pub struct RustGauge {
    inner: opentelemetry::metrics::Gauge<f64>,
}

impl std::fmt::Debug for RustGauge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustGauge").finish_non_exhaustive()
    }
}

#[pymethods]
impl RustGauge {
    /// Set the gauge value.
    #[pyo3(signature = (value, labels=None))]
    fn set(&self, value: f64, labels: Option<HashMap<String, String>>) {
        let attrs = labels_to_kv(labels);
        self.inner.record(value, &attrs);
    }
}

// ── Factory functions ───────────────────────────────────────────────────

/// Create a counter instrument.
#[pyfunction]
#[pyo3(signature = (name, description=String::new(), unit=String::new()))]
pub fn create_counter(name: String, description: String, unit: String) -> RustCounter {
    let meter = user_meter();
    let mut builder = meter.u64_counter(name);
    if !description.is_empty() {
        builder = builder.with_description(description);
    }
    if !unit.is_empty() {
        builder = builder.with_unit(unit);
    }
    RustCounter {
        inner: builder.build(),
    }
}

/// Create a histogram instrument.
#[pyfunction]
#[pyo3(signature = (name, description=String::new(), unit=String::new()))]
pub fn create_histogram(name: String, description: String, unit: String) -> RustHistogram {
    let meter = user_meter();
    let mut builder = meter.f64_histogram(name);
    if !description.is_empty() {
        builder = builder.with_description(description);
    }
    if !unit.is_empty() {
        builder = builder.with_unit(unit);
    }
    RustHistogram {
        inner: builder.build(),
    }
}

/// Create a gauge instrument.
#[pyfunction]
#[pyo3(signature = (name, description=String::new(), unit=String::new()))]
pub fn create_gauge(name: String, description: String, unit: String) -> RustGauge {
    let meter = user_meter();
    let mut builder = meter.f64_gauge(name);
    if !description.is_empty() {
        builder = builder.with_description(description);
    }
    if !unit.is_empty() {
        builder = builder.with_unit(unit);
    }
    RustGauge {
        inner: builder.build(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Convert optional label map to OTEL `KeyValue` vec.
///
/// Returns an empty vec without allocating when labels are absent.
fn labels_to_kv(labels: Option<HashMap<String, String>>) -> Vec<opentelemetry::KeyValue> {
    let Some(map) = labels else {
        return Vec::new();
    };
    map.into_iter()
        .map(|(k, v)| opentelemetry::KeyValue::new(k, v))
        .collect()
}
