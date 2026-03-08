use apx_common::tracing_fmt::{DevAwareFormatter, build_apx_filter};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

pub use apx_common::tracing_fmt::enable_dev_format;

/// Initialize the tracing subscriber with optional OTLP log export.
///
/// Reads `APX_LOG` for the log filter and `APX_OTEL_LOGS=1` to enable OTLP export.
pub fn init_tracing() {
    let filter = build_apx_filter("apx");

    let otel_enabled = std::env::var("APX_OTEL_LOGS").is_ok_and(|v| v == "1");
    let app_dir = std::env::var("APX_APP_DIR").ok();

    if otel_enabled {
        if let Err(e) = init_tracing_with_otel("apx", &filter, app_dir.as_deref()) {
            eprintln!("Warning: Failed to initialize OTLP logging: {e}");
            init_tracing_fmt_only(&filter);
        }
    } else {
        init_tracing_fmt_only(&filter);
    }
}

fn init_tracing_fmt_only(filter: &str) {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
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
    service_name: &str,
    filter: &str,
    app_dir: Option<&str>,
) -> Result<(), String> {
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::logs::SdkLoggerProvider;

    let endpoint = format!(
        "http://{}:{}/v1/logs",
        apx_common::hosts::CLIENT_HOST,
        crate::flux::FLUX_PORT
    );

    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| format!("Failed to create OTLP exporter: {e}"))?;

    let mut attributes = vec![KeyValue::new("service.name", service_name.to_string())];
    if let Some(app_path) = app_dir {
        attributes.push(KeyValue::new("apx.app_path", app_path.to_string()));
    }

    let provider = SdkLoggerProvider::builder()
        .with_resource(Resource::builder().with_attributes(attributes).build())
        .with_batch_exporter(exporter)
        .build();

    let otel_layer =
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider)
            .with_filter(EnvFilter::new(filter));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(DevAwareFormatter)
        .with_filter(EnvFilter::new(filter));

    if tracing_subscriber::registry()
        .with(otel_layer)
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Warning: tracing subscriber already initialized");
    }

    Ok(())
}
