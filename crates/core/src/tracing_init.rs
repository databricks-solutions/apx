//! Tracing / OTLP initialization with optional traces, metrics, and logs.
//!
//! | Env var | Purpose |
//! |---------|---------|
//! | `APX_OTEL=1` | Master enable for all signals |
//! | `APX_OTEL_TRACES=1` | Enable trace export |
//! | `APX_OTEL_METRICS=1` | Enable metric export |
//! | `APX_OTEL_LOGS=1` | Enable log export |
//! | `OTEL_EXPORTER_OTLP_ENDPOINT` | Base OTLP endpoint |
//! | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Override for traces |
//! | `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | Override for metrics |
//! | `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | Override for logs |
//! | `OTEL_SERVICE_NAME` | Service name resource attribute |

use apx_common::tracing_fmt::{DevAwareFormatter, build_apx_filter};
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use std::sync::OnceLock;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

pub use apx_common::tracing_fmt::enable_dev_format;

/// Default OTLP base endpoint when none is configured.
const DEFAULT_OTLP_ENDPOINT: &str = "http://localhost:4318";

/// Stored tracer provider — kept alive for the process lifetime.
static TRACER_PROVIDER: OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> = OnceLock::new();

/// Stored meter provider — kept alive for the process lifetime.
static METER_PROVIDER: OnceLock<opentelemetry_sdk::metrics::SdkMeterProvider> = OnceLock::new();

/// Stored logger provider — kept alive for the process lifetime.
static LOGGER_PROVIDER: OnceLock<opentelemetry_sdk::logs::SdkLoggerProvider> = OnceLock::new();

/// Access the global tracer provider (if OTEL traces are enabled).
pub fn tracer_provider() -> Option<&'static opentelemetry_sdk::trace::SdkTracerProvider> {
    TRACER_PROVIDER.get()
}

/// Access the global meter provider (if OTEL metrics are enabled).
pub fn meter_provider() -> Option<&'static opentelemetry_sdk::metrics::SdkMeterProvider> {
    METER_PROVIDER.get()
}

/// Initialize the tracing subscriber with optional OTLP export.
///
/// Reads `APX_LOG` for the log filter and `APX_OTEL*` env vars for OTLP signals.
pub fn init_tracing() {
    let filter = build_apx_filter("apx");
    let app_dir = std::env::var("APX_APP_DIR").ok();
    let signals = OtelSignals::from_env();

    if signals.any_enabled() {
        if let Err(e) = init_tracing_with_otel(&filter, app_dir.as_deref(), &signals) {
            eprintln!("Warning: Failed to initialize OTLP: {e}");
            init_tracing_fmt_only(&filter);
        }
    } else {
        init_tracing_fmt_only(&filter);
    }
}

/// Flush pending spans, metrics, and logs. Call before process exit.
pub fn shutdown_telemetry() {
    if let Some(tp) = TRACER_PROVIDER.get()
        && let Err(e) = tp.shutdown()
    {
        tracing::warn!("tracer provider shutdown: {e}");
    }
    if let Some(mp) = METER_PROVIDER.get()
        && let Err(e) = mp.shutdown()
    {
        tracing::warn!("meter provider shutdown: {e}");
    }
    if let Some(lp) = LOGGER_PROVIDER.get()
        && let Err(e) = lp.shutdown()
    {
        tracing::warn!("logger provider shutdown: {e}");
    }
}

// ── Internal ────────────────────────────────────────────────────────────

/// Which OTEL signals are enabled.
struct OtelSignals {
    traces: bool,
    metrics: bool,
    logs: bool,
}

impl OtelSignals {
    fn from_env() -> Self {
        let master = env_flag("APX_OTEL");
        Self {
            traces: master || env_flag("APX_OTEL_TRACES"),
            metrics: master || env_flag("APX_OTEL_METRICS"),
            logs: master || env_flag("APX_OTEL_LOGS"),
        }
    }

    fn any_enabled(&self) -> bool {
        self.traces || self.metrics || self.logs
    }
}

/// Check if an env var is set to "1".
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Read an env var with a fallback.
fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// Build the OTEL resource with service name and optional app path.
fn build_resource(app_dir: Option<&str>) -> Resource {
    let service_name = env_or("OTEL_SERVICE_NAME", "apx");
    let mut attrs = vec![KeyValue::new("service.name", service_name)];
    if let Some(path) = app_dir {
        attrs.push(KeyValue::new("apx.app_path", path.to_owned()));
    }
    Resource::builder().with_attributes(attrs).build()
}

/// Resolve the base OTLP endpoint.
fn base_endpoint() -> String {
    env_or("OTEL_EXPORTER_OTLP_ENDPOINT", DEFAULT_OTLP_ENDPOINT)
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
    }
}

fn init_tracing_with_otel(
    filter: &str,
    app_dir: Option<&str>,
    signals: &OtelSignals,
) -> Result<(), String> {
    use opentelemetry_otlp::WithExportConfig;

    let resource = build_resource(app_dir);
    let base = base_endpoint();

    let registry = tracing_subscriber::registry();

    // ── Traces ──────────────────────────────────────────────────────
    let otel_trace_layer = if signals.traces {
        let endpoint = env_or(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            &format!("{base}/v1/traces"),
        );
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(&endpoint)
            .build()
            .map_err(|e| format!("span exporter: {e}"))?;

        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(exporter)
            .build();

        let tracer = provider.tracer("apx-framework");
        let _ = TRACER_PROVIDER.set(provider);

        Some(
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(EnvFilter::new(filter)),
        )
    } else {
        None
    };

    // ── Metrics ─────────────────────────────────────────────────────
    if signals.metrics {
        let endpoint = env_or(
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            &format!("{base}/v1/metrics"),
        );
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(&endpoint)
            .build()
            .map_err(|e| format!("metric exporter: {e}"))?;

        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_periodic_exporter(exporter)
            .build();

        let _ = METER_PROVIDER.set(provider);
    }

    // ── Logs ────────────────────────────────────────────────────────
    let otel_log_layer = if signals.logs {
        let endpoint = env_or("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", &flux_logs_endpoint());
        let exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(&endpoint)
            .build()
            .map_err(|e| format!("log exporter: {e}"))?;

        let provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build();

        let layer =
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider)
                .with_filter(EnvFilter::new(filter));
        let _ = LOGGER_PROVIDER.set(provider);
        Some(layer)
    } else {
        None
    };

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
    }

    Ok(())
}

/// Default logs endpoint — Flux collector on localhost.
fn flux_logs_endpoint() -> String {
    format!(
        "http://{}:{}/v1/logs",
        apx_common::hosts::CLIENT_HOST,
        crate::flux::FLUX_PORT
    )
}
