//! OTEL utilities for sending logs to flux.
//!
//! This module provides shared functionality for building and sending OTLP log payloads
//! to the flux collector. Used by both subprocess log forwarding and browser log forwarding.

use std::path::Path;
use std::time::Duration;

use crate::flux::FLUX_PORT;

/// Convert severity level string to OTLP severity number.
fn severity_to_number(level: &str) -> u8 {
    match level.to_uppercase().as_str() {
        "TRACE" => 1,
        "DEBUG" => 5,
        "INFO" | "LOG" => 9,
        "WARN" | "WARNING" => 13,
        "ERROR" => 17,
        "FATAL" | "CRITICAL" => 21,
        _ => 9, // default to INFO
    }
}

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
    build_otlp_log_payload(message, level, timestamp_ns, service_name, &app_dir.display().to_string())
}

/// Forward a log line to flux via OTLP HTTP.
/// This is fire-and-forget; errors are silently ignored to avoid log loops.
pub async fn forward_log_to_flux(message: &str, level: &str, service_name: &str, app_path: &str) {
    // Skip noisy internal logs
    if should_skip_log(message) {
        return;
    }

    let timestamp_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let payload = build_otlp_log_payload(message, level, timestamp_ns, service_name, app_path);
    let endpoint = format!("http://127.0.0.1:{}/v1/logs", FLUX_PORT);

    // Use a simple HTTP client - fire and forget
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    let _ = client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await;
}

/// Check if a log message should be skipped (internal/noisy logs).
/// This filters out verbose debug output that would clutter the log stream.
pub fn should_skip_log(message: &str) -> bool {
    // HTTP connection pooling logs (hyper/reqwest)
    if message.starts_with("starting new connection:")
        || message.starts_with("connecting to ")
        || message.starts_with("connected to ")
        || message.starts_with("reuse idle connection")
        || message.starts_with("pooling idle connection")
    {
        return true;
    }

    // Tokio-postgres internal debug logs (may contain passwords)
    if message.starts_with("preparing query ")
        || message.starts_with("DEBUG: parse ")
        || message.starts_with("DEBUG: bind ")
        || message.starts_with("executing statement ")
    {
        return true;
    }

    // Skip messages containing sensitive data patterns
    if message.contains("WITH PASSWORD") || message.contains("PASSWORD '") {
        return true;
    }

    false
}
