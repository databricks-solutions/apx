//! Automatic HTTP server metrics and span attribute helpers.
//!
//! Records `http.server.request.duration` and `http.server.active_requests`
//! using OTEL semantic conventions v1.23+. When OTEL is disabled, the global
//! meter returns noop instruments — zero overhead automatically.
//!
//! Per-metric toggles are initialized once per worker process via [`init`]
//! after reading the Python telemetry config.

use std::sync::OnceLock;

use crate::protocol::http::error::AppError;
use crate::telemetry::config::HttpMetricToggles;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider;

// ── Global HTTP metric toggles ────────────────────────────────────────────

static HTTP_TOGGLES: OnceLock<HttpMetricToggles> = OnceLock::new();

/// Initialize HTTP metric toggles for this worker process.
///
/// Must be called once after reading the Python telemetry config.
/// Subsequent calls are silently ignored (OnceLock semantics).
pub fn init(toggles: HttpMetricToggles) {
    let _ = HTTP_TOGGLES.set(toggles);
}

/// Return the active HTTP metric toggles.
///
/// Falls back to all-enabled defaults if [`init`] has not been called.
fn http_toggles() -> &'static HttpMetricToggles {
    static DEFAULT: HttpMetricToggles = HttpMetricToggles {
        server_request_duration: true,
        server_active_requests: true,
    };
    HTTP_TOGGLES.get().unwrap_or(&DEFAULT)
}

// ── Framework meter ───────────────────────────────────────────────────────

/// Obtain the framework-internal meter.
///
/// Uses the configured provider if available, falls back to the global
/// (which may be noop when OTEL is disabled — zero overhead).
pub(crate) fn framework_meter() -> opentelemetry::metrics::Meter {
    static LOGGED: std::sync::Once = std::sync::Once::new();
    if let Some(mp) = apx_core::tracing_init::meter_provider() {
        LOGGED.call_once(|| {
            tracing::info!(target: "apx::telemetry", meter = "apx.framework", "framework meter: using configured SdkMeterProvider");
        });
        mp.meter("apx.framework")
    } else {
        LOGGED.call_once(|| {
            tracing::warn!(target: "apx::telemetry", meter = "apx.framework", "framework meter: SdkMeterProvider not initialized, using global noop");
        });
        opentelemetry::global::meter("apx.framework")
    }
}

/// RAII guard that decrements `http.server.active_requests` on drop.
///
/// Covers panics, timeouts, and early returns — the counter is always
/// decremented when the guard goes out of scope.
///
/// Returns `None` when the `server_active_requests` toggle is disabled.
#[derive(Debug)]
pub struct ActiveRequestGuard {
    attrs: [KeyValue; 2],
}

impl ActiveRequestGuard {
    /// Increment active requests and return a guard that decrements on drop.
    ///
    /// Returns `None` if the `server_active_requests` metric is disabled.
    pub fn enter(method: &str, scheme: &str) -> Option<Self> {
        if !http_toggles().server_active_requests {
            return None;
        }
        let attrs = [
            KeyValue::new("http.request.method", method.to_owned()),
            KeyValue::new("url.scheme", scheme.to_owned()),
        ];
        framework_meter()
            .i64_up_down_counter("http.server.active_requests")
            .build()
            .add(1, &attrs);
        Some(Self { attrs })
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
///
/// No-ops when the `server_request_duration` metric is disabled.
pub fn record_duration(
    duration_secs: f64,
    method: &str,
    scheme: &str,
    status_code: u16,
    route: &str,
    error_type: Option<&str>,
) {
    if !http_toggles().server_request_duration {
        return;
    }

    static FIRST: std::sync::Once = std::sync::Once::new();

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

    FIRST.call_once(|| {
        tracing::info!(
            target: "apx::telemetry",
            method,
            status_code,
            route,
            duration_ms = format_args!("{:.1}", duration_secs * 1000.0),
            "http metrics: first request duration recorded"
        );
    });
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

// ── Header capture ───────────────────────────────────────────────────────

use super::config::HttpConfig;

/// Captured header attribute following OTEL semconv `http.{request,response}.header.<name>`.
fn header_attr_name(direction: &str, name: &str) -> String {
    let normalized = name.to_lowercase().replace('-', "_");
    format!("http.{direction}.header.{normalized}")
}

fn is_sanitized(name: &str, patterns: &[String]) -> bool {
    let lower = name.to_lowercase();
    patterns.iter().any(|p| lower.contains(&p.to_lowercase()))
}

const REDACTED: &str = "[REDACTED]";

/// Extract request header values as OTEL span attributes.
pub fn capture_request_headers(headers: &http::HeaderMap, config: &HttpConfig) -> Vec<KeyValue> {
    let mut attrs = Vec::new();
    for name in &config.capture_request_headers {
        let lower = name.to_lowercase();
        let values: Vec<&str> = headers
            .get_all(
                http::header::HeaderName::from_bytes(lower.as_bytes())
                    .unwrap_or(http::header::HeaderName::from_static("x-unknown")),
            )
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        if values.is_empty() {
            continue;
        }
        let attr_name = header_attr_name("request", name);
        let value = if is_sanitized(name, &config.sanitize_headers) {
            REDACTED.to_owned()
        } else {
            values.join(", ")
        };
        attrs.push(KeyValue::new(attr_name, value));
    }
    attrs
}

/// Extract response header values as OTEL span attributes.
pub fn capture_response_headers(headers: &http::HeaderMap, config: &HttpConfig) -> Vec<KeyValue> {
    let mut attrs = Vec::new();
    for name in &config.capture_response_headers {
        let lower = name.to_lowercase();
        let values: Vec<&str> = headers
            .get_all(
                http::header::HeaderName::from_bytes(lower.as_bytes())
                    .unwrap_or(http::header::HeaderName::from_static("x-unknown")),
            )
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        if values.is_empty() {
            continue;
        }
        let attr_name = header_attr_name("response", name);
        let value = if is_sanitized(name, &config.sanitize_headers) {
            REDACTED.to_owned()
        } else {
            values.join(", ")
        };
        attrs.push(KeyValue::new(attr_name, value));
    }
    attrs
}
