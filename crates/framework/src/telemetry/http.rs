//! Automatic HTTP server metrics and span attribute helpers.
//!
//! Records `http.server.request.duration` and `http.server.active_requests`
//! using OTEL semantic conventions v1.23+. When OTEL is disabled, the global
//! meter returns noop instruments — zero overhead automatically.

use crate::protocol::http::error::AppError;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;

/// Obtain the framework-internal meter.
///
/// Uses the configured provider if available, falls back to the global
/// (which may be noop when OTEL is disabled — zero overhead).
fn framework_meter() -> opentelemetry::metrics::Meter {
    apx_core::tracing_init::meter_provider().map_or_else(
        || opentelemetry::global::meter("apx.framework"),
        |mp| mp.meter("apx.framework"),
    )
}

/// RAII guard that decrements `http.server.active_requests` on drop.
///
/// Covers panics, timeouts, and early returns — the counter is always
/// decremented when the guard goes out of scope.
#[derive(Debug)]
pub struct ActiveRequestGuard {
    attrs: [KeyValue; 2],
}

impl ActiveRequestGuard {
    /// Increment active requests and return a guard that decrements on drop.
    pub fn enter(method: &str, scheme: &str) -> Self {
        let attrs = [
            KeyValue::new("http.request.method", method.to_owned()),
            KeyValue::new("url.scheme", scheme.to_owned()),
        ];
        framework_meter()
            .i64_up_down_counter("http.server.active_requests")
            .build()
            .add(1, &attrs);
        Self { attrs }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        framework_meter()
            .i64_up_down_counter("http.server.active_requests")
            .build()
            .add(-1, &self.attrs);
    }
}

/// Record `http.server.request.duration` with standard attributes.
pub fn record_duration(
    duration_secs: f64,
    method: &str,
    scheme: &str,
    status_code: u16,
    route: &str,
    error_type: Option<&str>,
) {
    let mut attrs = vec![
        KeyValue::new("http.request.method", method.to_owned()),
        KeyValue::new("url.scheme", scheme.to_owned()),
        KeyValue::new("http.response.status_code", i64::from(status_code)),
        KeyValue::new("http.route", route.to_owned()),
    ];
    if let Some(et) = error_type {
        attrs.push(KeyValue::new("error.type", et.to_owned()));
    }
    framework_meter()
        .f64_histogram("http.server.request.duration")
        .with_description("Duration of HTTP server requests")
        .with_unit("s")
        .build()
        .record(duration_secs, &attrs);
}

/// Map an `AppError` variant to an OTEL semconv `error.type` value.
pub fn error_type_for(err: &AppError) -> &'static str {
    match err {
        AppError::Internal(_) => "500",
        AppError::Timeout => "408",
    }
}

/// Map `http::Version` to the semconv `network.protocol.version` string.
pub fn protocol_version(version: http::Version) -> &'static str {
    match version {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => "1.1",
    }
}
