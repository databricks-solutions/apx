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
use crate::telemetry::defs;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, UpDownCounter};

// ── Global HTTP metric toggles ────────────────────────────────────────────

super::toggle_store!(HTTP_TOGGLES: HttpMetricToggles = HttpMetricToggles {
    server_request_duration: true,
    server_active_requests: true,
});

// ── Framework meter ───────────────────────────────────────────────────────

/// Obtain the framework-internal meter (`apx.framework`).
pub(crate) fn framework_meter() -> opentelemetry::metrics::Meter {
    super::get_meter("apx.framework")
}

// ── Active requests instrument ────────────────────────────────────────────

fn active_requests_counter() -> &'static UpDownCounter<i64> {
    static COUNTER: OnceLock<UpDownCounter<i64>> = OnceLock::new();
    COUNTER.get_or_init(|| {
        framework_meter()
            .i64_up_down_counter(defs::HTTP_ACTIVE_REQUESTS.name)
            .with_description(defs::HTTP_ACTIVE_REQUESTS.description)
            .with_unit(defs::HTTP_ACTIVE_REQUESTS.unit)
            .build()
    })
}

// ── Active requests guard ─────────────────────────────────────────────────

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
        if !toggles().server_active_requests {
            return None;
        }
        let attrs = [
            KeyValue::new("http.request.method", method.to_owned()),
            KeyValue::new("url.scheme", scheme.to_owned()),
        ];
        active_requests_counter().add(1, &attrs);
        Some(Self { attrs })
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        active_requests_counter().add(-1, &self.attrs);
    }
}

// ── Request duration instrument ───────────────────────────────────────────

fn duration_histogram() -> &'static Histogram<f64> {
    static HIST: OnceLock<Histogram<f64>> = OnceLock::new();
    HIST.get_or_init(|| defs::HTTP_REQUEST_DURATION.histogram(&framework_meter()))
}

// ── Request duration ──────────────────────────────────────────────────────

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
    if !toggles().server_request_duration {
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
    duration_histogram().record(duration_secs, &attrs);

    FIRST.call_once(|| {
        tracing::info!(
            name: "apx.http.first_request_recorded",
            target: "apx::telemetry",
            method,
            status_code,
            route,
            duration_ms = format_args!("{:.1}", duration_secs * 1000.0),
            "http metrics: first request duration recorded"
        );
    });
}

// ── Error / protocol helpers ──────────────────────────────────────────────

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

const REDACTED: &str = "[REDACTED]";

/// Extract header values as OTEL span attributes for the given direction.
fn capture_headers(
    direction: &str,
    header_names: &[String],
    headers: &http::HeaderMap,
    sanitize_patterns: &[String],
) -> Vec<KeyValue> {
    let mut attrs = Vec::new();
    for name in header_names {
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
        let normalized = name.to_lowercase().replace('-', "_");
        let attr_name = format!("http.{direction}.header.{normalized}");
        let value = if sanitize_patterns
            .iter()
            .any(|p| lower.contains(&p.to_lowercase()))
        {
            REDACTED.to_owned()
        } else {
            values.join(", ")
        };
        attrs.push(KeyValue::new(attr_name, value));
    }
    attrs
}

/// Extract request header values as OTEL span attributes.
pub fn capture_request_headers(headers: &http::HeaderMap, config: &HttpConfig) -> Vec<KeyValue> {
    capture_headers(
        "request",
        &config.capture_request_headers,
        headers,
        &config.sanitize_headers,
    )
}

/// Extract response header values as OTEL span attributes.
pub fn capture_response_headers(headers: &http::HeaderMap, config: &HttpConfig) -> Vec<KeyValue> {
    capture_headers(
        "response",
        &config.capture_response_headers,
        headers,
        &config.sanitize_headers,
    )
}
