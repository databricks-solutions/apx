//! OTEL utilities for sending logs to flux.
//!
//! This module provides shared functionality for building and sending OTLP log payloads
//! to the flux collector. Used by both subprocess log forwarding and browser log forwarding.

use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use apx_common::format::severity_to_number;
use apx_common::hosts::CLIENT_HOST;

use crate::flux::FLUX_PORT;

/// Shared HTTP client for forwarding logs to flux.
/// Reused across all calls to avoid creating a new client per log line.
static FLUX_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .pool_max_idle_per_host(2)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Build an OTLP JSON log payload with nanosecond timestamp.
pub fn build_otlp_log_payload(
    message: &str,
    level: &str,
    timestamp_ns: i64,
    service_name: &str,
    app_path: &str,
) -> serde_json::Value {
    let severity_number = severity_to_number(level);

    serde_json::json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [
                    {
                        "key": "service.name",
                        "value": { "stringValue": service_name }
                    },
                    {
                        "key": "apx.app_path",
                        "value": { "stringValue": app_path }
                    }
                ]
            },
            "scopeLogs": [{
                "scope": {},
                "logRecords": [{
                    "timeUnixNano": timestamp_ns.to_string(),
                    "severityNumber": severity_number,
                    "severityText": level.to_uppercase(),
                    "body": { "stringValue": message }
                }]
            }]
        }]
    })
}

/// Build an OTLP JSON log payload from millisecond timestamp.
/// Convenience wrapper for browser logs which use milliseconds.
pub fn build_otlp_log_payload_from_ms(
    message: &str,
    level: &str,
    timestamp_ms: i64,
    service_name: &str,
    app_dir: &Path,
) -> serde_json::Value {
    let timestamp_ns = timestamp_ms * 1_000_000;
    build_otlp_log_payload(
        message,
        level,
        timestamp_ns,
        service_name,
        &app_dir.display().to_string(),
    )
}

/// Forward a log line to flux via OTLP HTTP.
/// This is fire-and-forget; errors are silently ignored to avoid log loops.
pub async fn forward_log_to_flux(message: &str, level: &str, service_name: &str, app_path: &str) {
    // Skip noisy internal logs
    if apx_common::should_skip_log_message(message) {
        return;
    }

    let timestamp_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let payload = build_otlp_log_payload(message, level, timestamp_ns, service_name, app_path);
    let endpoint = format!("http://{CLIENT_HOST}:{FLUX_PORT}/v1/logs");

    let _ = FLUX_CLIENT
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await;
}
