//! Tracing / OTLP initialization — gRPC only.
//!
//! Telemetry is enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//! All three signals (traces, metrics, logs) are exported to that
//! endpoint via gRPC (tonic). The only supported value for
//! `OTEL_EXPORTER_OTLP_PROTOCOL` is `grpc` (default when unset).
//!
//! | Env var | Purpose |
//! |---------|---------|
//! | `OTEL_EXPORTER_OTLP_ENDPOINT` | Base gRPC endpoint (enables OTEL) |
//! | `OTEL_EXPORTER_OTLP_PROTOCOL` | Must be `grpc` or absent |
//! | `OTEL_SERVICE_NAME` | Service name resource attribute |
//! | `OTEL_RESOURCE_ATTRIBUTES` | Comma-separated `key=value` pairs |
//! | `OTEL_BSP_*` | Batch span processor tuning (SDK-native) |
//! | `OTEL_BLRP_*` | Batch log record processor tuning (SDK-native) |

use apx_common::tracing_fmt::{DevAwareFormatter, build_apx_filter};
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use std::sync::OnceLock;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

pub use apx_common::tracing_fmt::enable_dev_format;

/// Stored tracer provider — kept alive for the process lifetime.
static TRACER_PROVIDER: OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> = OnceLock::new();

/// Stored meter provider — kept alive for the process lifetime.
static METER_PROVIDER: OnceLock<opentelemetry_sdk::metrics::SdkMeterProvider> = OnceLock::new();

/// Stored logger provider — kept alive for the process lifetime.
static LOGGER_PROVIDER: OnceLock<opentelemetry_sdk::logs::SdkLoggerProvider> = OnceLock::new();

/// Access the global tracer provider (if OTEL is enabled).
pub fn tracer_provider() -> Option<&'static opentelemetry_sdk::trace::SdkTracerProvider> {
    TRACER_PROVIDER.get()
}

/// Access the global meter provider (if OTEL is enabled).
pub fn meter_provider() -> Option<&'static opentelemetry_sdk::metrics::SdkMeterProvider> {
    METER_PROVIDER.get()
}

/// Initialize the tracing subscriber with optional OTLP export.
///
/// OTEL is enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set and
/// `OTEL_EXPORTER_OTLP_PROTOCOL` is `grpc` (or absent).
pub fn init_tracing() {
    let filter = build_apx_filter("apx");
    let app_dir = std::env::var("APX_APP_DIR").ok();

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty());

    if let Some(endpoint) = endpoint {
        let protocol =
            std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").unwrap_or_else(|_| "grpc".to_owned());

        if protocol != "grpc" {
            eprintln!(
                "Warning: OTEL_EXPORTER_OTLP_PROTOCOL={protocol:?} is not supported (only \"grpc\"); \
                 falling back to fmt-only logging"
            );
            init_tracing_fmt_only(&filter);
            return;
        }

        if let Err(e) = init_tracing_with_otel(&filter, app_dir.as_deref(), &endpoint) {
            eprintln!("Warning: Failed to initialize OTLP: {e}");
            init_tracing_fmt_only(&filter);
        }
    } else {
        init_tracing_fmt_only(&filter);
    }
}

/// Flush pending spans, metrics, and logs. Call before process exit.
pub fn shutdown_telemetry() {
    tracing::info!(target: "apx::telemetry", "flushing OTLP providers");
    if let Some(tp) = TRACER_PROVIDER.get() {
        match tp.shutdown() {
            Ok(()) => tracing::info!(target: "apx::telemetry", "tracer provider flushed"),
            Err(e) => tracing::warn!("tracer provider shutdown: {e}"),
        }
    } else {
        tracing::info!(target: "apx::telemetry", "tracer provider not initialized, skipping flush");
    }
    if let Some(mp) = METER_PROVIDER.get() {
        match mp.shutdown() {
            Ok(()) => tracing::info!(target: "apx::telemetry", "meter provider flushed"),
            Err(e) => tracing::warn!("meter provider shutdown: {e}"),
        }
    } else {
        tracing::info!(target: "apx::telemetry", "meter provider not initialized, skipping flush");
    }
    if let Some(lp) = LOGGER_PROVIDER.get() {
        match lp.shutdown() {
            Ok(()) => tracing::info!(target: "apx::telemetry", "logger provider flushed"),
            Err(e) => tracing::warn!("logger provider shutdown: {e}"),
        }
    } else {
        tracing::info!(target: "apx::telemetry", "logger provider not initialized, skipping flush");
    }
}

// ── Internal ────────────────────────────────────────────────────────────

/// OpenTelemetry semantic conventions version for resource attributes.
const SEMCONV_SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.29.0";

/// Histogram boundaries for duration metrics recorded in seconds.
///
/// Aligned with OpenTelemetry HTTP semantic conventions for
/// `http.server.request.duration`.
const DURATION_SECONDS_BOUNDARIES: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Histogram boundaries for duration metrics recorded in microseconds.
///
/// Covers sub-millisecond dispatch latencies (body collect, crossbeam send,
/// ASGI parse) up to 100ms outliers.
const DURATION_MICROSECONDS_BOUNDARIES: &[f64] = &[
    1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0, 2_500.0, 5_000.0, 10_000.0, 50_000.0,
    100_000.0,
];

/// SDK View that assigns appropriate histogram bucket boundaries based on unit.
///
/// Matches histogram instruments by their declared unit (`"s"` or `"us"`) and
/// overrides the default SDK boundaries (which assume milliseconds) with
/// boundaries that match the actual measurement scale.
fn histogram_bucket_view(
    inst: &opentelemetry_sdk::metrics::Instrument,
) -> Option<opentelemetry_sdk::metrics::Stream> {
    use opentelemetry_sdk::metrics::{Aggregation, InstrumentKind, Stream};

    if inst.kind != Some(InstrumentKind::Histogram) {
        return None;
    }

    let boundaries = match inst.unit.as_ref() {
        "s" => DURATION_SECONDS_BOUNDARIES,
        "us" => DURATION_MICROSECONDS_BOUNDARIES,
        _ => return None,
    };

    Some(
        Stream::new().aggregation(Aggregation::ExplicitBucketHistogram {
            boundaries: boundaries.to_vec(),
            record_min_max: true,
        }),
    )
}

/// Build the OTEL resource from `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`,
/// and the optional `APX_APP_DIR`.
fn build_resource(app_dir: Option<&str>) -> Resource {
    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "apx".to_owned());
    let mut attrs = vec![KeyValue::new("service.name", service_name)];

    if let Ok(raw) = std::env::var("OTEL_RESOURCE_ATTRIBUTES") {
        for pair in raw.split(',') {
            let pair = pair.trim();
            if let Some((k, v)) = pair.split_once('=') {
                attrs.push(KeyValue::new(k.to_owned(), v.to_owned()));
            }
        }
    }

    if let Some(path) = app_dir {
        attrs.push(KeyValue::new("apx.app_path", path.to_owned()));
    }

    if let Ok(worker_id) = std::env::var("APX_WORKER_ID") {
        attrs.push(KeyValue::new("apx.role", "worker"));
        attrs.push(KeyValue::new("apx.worker.id", worker_id));
    } else if std::env::var_os("APX_WORKER_NONCE").is_none() {
        attrs.push(KeyValue::new("apx.role", "supervisor"));
    }

    Resource::builder()
        .with_schema_url(attrs, SEMCONV_SCHEMA_URL)
        .build()
}

fn init_tracing_fmt_only(filter: &str) {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .event_format(DevAwareFormatter)
        .with_filter(EnvFilter::new(filter));

    if tracing_subscriber::registry()
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Warning: tracing subscriber already initialized");
    } else {
        tracing::info!(
            target: "apx::telemetry",
            filter,
            "fmt-only tracing active (OTEL_EXPORTER_OTLP_ENDPOINT not set)"
        );
    }
}

fn init_tracing_with_otel(
    filter: &str,
    app_dir: Option<&str>,
    endpoint: &str,
) -> Result<(), String> {
    use opentelemetry_otlp::WithExportConfig;

    let resource = build_resource(app_dir);
    let resource_clone = resource.clone();
    let resource_attrs: Vec<_> = resource_clone.iter().collect();

    let registry = tracing_subscriber::registry();

    // ── Traces ──────────────────────────────────────────────────────
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| format!("span exporter: {e}"))?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("apx-framework");
    opentelemetry::global::set_tracer_provider(provider.clone());
    let _ = TRACER_PROVIDER.set(provider);

    let otel_trace_layer = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_filter(EnvFilter::new(filter));

    // ── Metrics ─────────────────────────────────────────────────────
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| format!("metric exporter: {e}"))?;

    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
        .with_interval(std::time::Duration::from_secs(10))
        .build();

    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_reader(reader)
        .with_view(histogram_bucket_view)
        .build();

    opentelemetry::global::set_meter_provider(provider.clone());
    let _ = METER_PROVIDER.set(provider);

    // ── Logs ────────────────────────────────────────────────────────
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| format!("log exporter: {e}"))?;

    let provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    let otel_log_layer =
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider)
            .with_filter(EnvFilter::new(filter));
    let _ = LOGGER_PROVIDER.set(provider);

    // ── Fmt (always) ────────────────────────────────────────────────
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .event_format(DevAwareFormatter)
        .with_filter(EnvFilter::new(filter));

    if registry
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Warning: tracing subscriber already initialized");
    } else {
        tracing::info!(
            target: "apx::telemetry",
            endpoint,
            filter,
            resource = ?resource_attrs,
            "OTLP pipeline active: traces + metrics + logs (10s metric interval)"
        );
    }

    Ok(())
}
