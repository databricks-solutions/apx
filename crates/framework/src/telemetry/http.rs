//! Automatic HTTP server metrics and span attribute helpers.
//!
//! Records `http.server.request.duration` and `http.server.active_requests`
//! using OTEL semantic conventions v1.23+. When OTEL is disabled, the global
//! meter returns noop instruments — zero overhead automatically.
//!
//! Per-metric toggles are initialized once per worker process via [`init`]
//! after reading the Python telemetry config.

use std::sync::OnceLock;

use crate::telemetry::config::HttpMetricToggles;
use crate::telemetry::context::TraceContext;
use crate::telemetry::defs;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, UpDownCounter};
use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};

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

// ── Instruments ──────────────────────────────────────────────────────────

fn duration_histogram() -> &'static Histogram<f64> {
    static HIST: OnceLock<Histogram<f64>> = OnceLock::new();
    HIST.get_or_init(|| defs::HTTP_REQUEST_DURATION.histogram(&framework_meter()))
}

fn active_requests_counter() -> &'static UpDownCounter<i64> {
    static CTR: OnceLock<UpDownCounter<i64>> = OnceLock::new();
    CTR.get_or_init(|| defs::HTTP_ACTIVE_REQUESTS.up_down_counter(&framework_meter()))
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
        tracing::debug!(
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

// ── Active requests ─────────────────────────────────────────────────────

/// Increment the `http.server.active_requests` counter.
pub fn inc_active_requests() {
    if toggles().server_active_requests {
        active_requests_counter().add(1, &[]);
    }
}

/// Decrement the `http.server.active_requests` counter.
pub fn dec_active_requests() {
    if toggles().server_active_requests {
        active_requests_counter().add(-1, &[]);
    }
}

// ── Request span ────────────────────────────────────────────────────────

/// Parse a UUID string into a 16-byte OTEL trace ID.
fn uuid_to_trace_id(uuid: &str) -> Option<[u8; 16]> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    hex::decode(&hex).ok()?.try_into().ok()
}

/// Create a `tracing` span for an HTTP request.
///
/// Uses the `x-request-id` UUID as the trace ID so that all spans
/// and logs within a request share the same trace. Returns the span
/// and a [`TraceContext`] for propagation to Python.
///
/// The returned span must be entered (via `.enter()`) when recording
/// metrics or logs that should carry the request's trace context.
pub fn begin_request_span(
    request_id: &str,
    method: &str,
    path: &str,
) -> (tracing::Span, TraceContext) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::info_span!(
        "http.server.request",
        "http.request.method" = method,
        "url.path" = path,
        "http.response.status_code" = tracing::field::Empty,
        otel.kind = "server",
    );

    if let Some(tid) = uuid_to_trace_id(request_id) {
        let parent_sc = SpanContext::new(
            TraceId::from_bytes(tid),
            SpanId::from_bytes(rand::random()),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let parent_cx = opentelemetry::Context::new().with_remote_span_context(parent_sc);
        span.set_parent(parent_cx);
    }

    let ctx = {
        let _guard = span.enter();
        super::context::extract_trace_context().unwrap_or(TraceContext {
            trace_id: [0; 16],
            span_id: [0; 8],
            trace_flags: 0,
            trace_state: String::new(),
        })
    };

    (span, ctx)
}

/// Record response status on a request span before it ends.
pub fn finish_request_span(span: &tracing::Span, status: u16) {
    span.record("http.response.status_code", i64::from(status));
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test code uses expect for clarity")]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_to_trace_id_valid() {
        let id = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
        let tid = uuid_to_trace_id(id).expect("valid UUID");
        assert_eq!(hex::encode(tid), "a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d");
    }

    #[test]
    fn test_uuid_to_trace_id_no_dashes() {
        let id = "a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d";
        let tid = uuid_to_trace_id(id).expect("valid hex");
        assert_eq!(hex::encode(tid), id);
    }

    #[test]
    fn test_uuid_to_trace_id_invalid() {
        assert!(uuid_to_trace_id("not-a-uuid").is_none());
        assert!(uuid_to_trace_id("").is_none());
        assert!(uuid_to_trace_id("zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz").is_none());
    }
}
