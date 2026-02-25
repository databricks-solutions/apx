use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::FormatEvent;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// When `true`, the fmt layer uses the dev-friendly `| apx | out/err |` format
/// instead of the default verbose format with target/file/line.
static DEV_FORMAT: AtomicBool = AtomicBool::new(false);

/// Enable the dev-friendly log format for attached mode.
///
/// Once called, all subsequent tracing events will be formatted as:
/// `YYYY-MM-DD HH:MM:SS.mmm | apx | out | message`
pub fn enable_dev_format() {
    DEV_FORMAT.store(true, Ordering::Relaxed);
}

const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RESET: &str = "\x1b[0m";

/// A tracing event formatter that switches between dev-friendly and verbose formats
/// based on the [`DEV_FORMAT`] flag.
struct DevAwareFormatter;

impl<S, N> FormatEvent<S, N> for DevAwareFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if DEV_FORMAT.load(Ordering::Relaxed) {
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            let channel = if *event.metadata().level() == tracing::Level::ERROR {
                "err"
            } else {
                "out"
            };

            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            let message = visitor.0;

            if writer.has_ansi_escapes() {
                writeln!(
                    writer,
                    "{ANSI_YELLOW}{timestamp} |  apx | {channel} | {message}{ANSI_RESET}"
                )
            } else {
                writeln!(writer, "{timestamp} |  apx | {channel} | {message}")
            }
        } else {
            // Verbose format with target, file, and line number
            use tracing_subscriber::fmt::time::FormatTime;
            use tracing_subscriber::fmt::time::SystemTime;

            let timer = SystemTime;
            timer.format_time(&mut writer)?;

            let level = event.metadata().level();
            write!(writer, " {level:>5} ")?;

            let target = event.metadata().target();
            write!(writer, "{target}: ")?;

            if let (Some(file), Some(line)) = (event.metadata().file(), event.metadata().line()) {
                write!(writer, "{file}:{line}: ")?;
            }

            ctx.format_fields(writer.by_ref(), event)?;
            writeln!(writer)
        }
    }
}

/// Visitor that extracts the message field from a tracing event.
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

/// Initialize the tracing subscriber with optional OTLP log export.
///
/// Reads `APX_LOG` for the log filter and `APX_OTEL_LOGS=1` to enable OTLP export.
pub fn init_tracing() {
    let apx_root = "apx";

    let filter = match std::env::var("APX_LOG") {
        Ok(level) if is_plain_level(&level) => {
            format!("{apx_root}={level}")
        }
        Ok(spec) => spec,
        Err(_) => format!("{apx_root}=info"),
    };

    let otel_enabled = std::env::var("APX_OTEL_LOGS").is_ok_and(|v| v == "1");
    let app_dir = std::env::var("APX_APP_DIR").ok();

    if otel_enabled {
        if let Err(e) = init_tracing_with_otel(apx_root, &filter, app_dir.as_deref()) {
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

fn is_plain_level(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    )
}
