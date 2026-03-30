//! OTLP metrics instruments exposed to Python via PyO3.
//!
//! Thin wrappers around `opentelemetry::metrics` instruments.
//! When OTEL is disabled, the global meter returns noop instruments
//! — zero overhead automatically.

use opentelemetry::metrics::Meter;
use pyo3::prelude::*;
use std::collections::HashMap;

/// Obtain the user-facing meter (backed by configured provider or global noop).
fn user_meter() -> Meter {
    super::get_meter("apx.user")
}

// ── Counter ─────────────────────────────────────────────────────────────

/// An OTLP counter metric.
#[pyclass(module = "apx._core")]
pub struct RustCounter {
    inner: opentelemetry::metrics::Counter<u64>,
}

crate::opaque_debug!(RustCounter);

#[pymethods]
impl RustCounter {
    /// Increment the counter.
    #[pyo3(signature = (value=1, attributes=None))]
    fn inc(&self, value: u64, attributes: Option<HashMap<String, String>>) {
        let attrs = to_kv(attributes);
        self.inner.add(value, &attrs);
    }
}

// ── Histogram ───────────────────────────────────────────────────────────

/// An OTLP histogram metric.
#[pyclass(module = "apx._core")]
pub struct RustHistogram {
    inner: opentelemetry::metrics::Histogram<f64>,
}

crate::opaque_debug!(RustHistogram);

#[pymethods]
impl RustHistogram {
    /// Record an observation.
    #[pyo3(signature = (value, attributes=None))]
    fn observe(&self, value: f64, attributes: Option<HashMap<String, String>>) {
        let attrs = to_kv(attributes);
        self.inner.record(value, &attrs);
    }
}

// ── Gauge ───────────────────────────────────────────────────────────────

/// An OTLP gauge metric.
#[pyclass(module = "apx._core")]
pub struct RustGauge {
    inner: opentelemetry::metrics::Gauge<f64>,
}

crate::opaque_debug!(RustGauge);

#[pymethods]
impl RustGauge {
    /// Set the gauge value.
    #[pyo3(signature = (value, attributes=None))]
    fn set(&self, value: f64, attributes: Option<HashMap<String, String>>) {
        let attrs = to_kv(attributes);
        self.inner.record(value, &attrs);
    }
}

// ── Factory functions ───────────────────────────────────────────────────

/// Create a counter instrument.
#[pyfunction]
#[pyo3(signature = (name, description=String::new(), unit=String::new()))]
pub fn create_counter(name: String, description: String, unit: String) -> RustCounter {
    tracing::trace!(name: "apx.telemetry.metric.counter_created", target: "apx::telemetry", name, unit, "creating user counter");
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
    tracing::trace!(name: "apx.telemetry.metric.histogram_created", target: "apx::telemetry", name, unit, "creating user histogram");
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
    tracing::trace!(name: "apx.telemetry.metric.gauge_created", target: "apx::telemetry", name, unit, "creating user gauge");
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

// ── Metric catalog introspection ─────────────────────────────────────────

/// A framework metric definition exposed to Python.
#[derive(Debug, Clone, Copy)]
#[pyclass(module = "apx._core", skip_from_py_object)]
pub struct PyMetricDefinition {
    /// OTEL metric name (e.g. `"system.cpu.simple_utilization"`).
    #[pyo3(get)]
    pub name: &'static str,
    /// Human-readable description.
    #[pyo3(get)]
    pub description: &'static str,
    /// UCUM unit string.
    #[pyo3(get)]
    pub unit: &'static str,
    /// Logical group: `"system"`, `"process"`, `"http"`, or `"apx"`.
    #[pyo3(get)]
    pub group: &'static str,
    /// Collection scope: `"supervisor"`, `"worker"`, or `"both"`.
    #[pyo3(get)]
    pub scope: &'static str,
}

#[pymethods]
impl PyMetricDefinition {
    fn __repr__(&self) -> String {
        format!(
            "MetricDefinition(name={:?}, group={:?}, scope={:?})",
            self.name, self.group, self.scope
        )
    }
}

/// Return the full catalog of framework-defined metrics.
#[pyfunction]
pub fn metric_catalog() -> Vec<PyMetricDefinition> {
    super::defs::ALL_METRICS
        .iter()
        .map(|entry| PyMetricDefinition {
            name: entry.def.name,
            description: entry.def.description,
            unit: entry.def.unit,
            group: entry.group,
            scope: entry.scope,
        })
        .collect()
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Convert optional attribute map to OTEL `KeyValue` vec.
///
/// Returns an empty vec without allocating when attributes are absent.
fn to_kv(attrs: Option<HashMap<String, String>>) -> Vec<opentelemetry::KeyValue> {
    let Some(map) = attrs else {
        return Vec::new();
    };
    map.into_iter()
        .map(|(k, v)| opentelemetry::KeyValue::new(k, v))
        .collect()
}
