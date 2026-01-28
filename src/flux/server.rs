//! OTLP HTTP receiver server for flux.
//!
//! This module implements an Axum HTTP server that receives OpenTelemetry logs
//! via OTLP HTTP protocol, supporting both JSON and Protobuf content types.

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use prost::Message;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{debug, error, info};

use super::storage::{LogRecord, Storage};

/// Flux port for OTLP HTTP receiver
pub const FLUX_PORT: u16 = 11111;

/// Application state shared across handlers.
#[derive(Clone)]
struct AppState {
    storage: Storage,
}

/// Run the flux server (entry point for `apx flux __run`).
///
/// This function initializes storage, starts the cleanup scheduler,
/// and runs the HTTP server. It blocks forever (or until error).
pub async fn run_server() -> Result<(), String> {
    // Log startup
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    eprintln!("[{}] Flux daemon starting...", now);

    // Open storage
    let storage = Storage::open()?;
    eprintln!("[{}] Storage initialized", now);

    // Start cleanup scheduler as a background task
    let storage_for_cleanup = storage.clone();
    tokio::spawn(async move {
        run_cleanup_loop(storage_for_cleanup).await;
    });

    // Run the HTTP server
    run_http_server(storage).await
}

/// Periodic cleanup loop that runs within the daemon process.
/// Deletes logs older than 7 days every hour.
async fn run_cleanup_loop(storage: Storage) {
    // Cleanup interval: 1 hour
    let interval = Duration::from_secs(60 * 60);

    info!("Cleanup scheduler started (interval: 1 hour, retention: 7 days)");

    // Run initial cleanup
    match storage.cleanup_old_logs() {
        Ok(deleted) if deleted > 0 => info!("Initial cleanup: removed {} old log records", deleted),
        Ok(_) => debug!("Initial cleanup: no old records to remove"),
        Err(e) => error!("Initial cleanup failed: {}", e),
    }

    loop {
        tokio::time::sleep(interval).await;

        match storage.cleanup_old_logs() {
            Ok(deleted) if deleted > 0 => {
                info!("Cleanup: removed {} old log records", deleted);
            }
            Ok(_) => {
                debug!("Cleanup: no old records to remove");
            }
            Err(e) => {
                error!("Cleanup failed: {}", e);
            }
        }
    }
}

/// Start the flux HTTP server with the given storage.
async fn run_http_server(storage: Storage) -> Result<(), String> {
    let state = AppState { storage };

    let app = Router::new()
        .route("/v1/logs", post(handle_logs))
        .route("/health", get(health_check))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", FLUX_PORT);
    info!("Starting flux OTLP receiver on {}", addr);

    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Failed to bind to {}: {}", addr, e))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Server error: {}", e))?;

    Ok(())
}

/// Health check endpoint.
async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}

/// Handle incoming OTLP logs (JSON or Protobuf).
async fn handle_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    let records = if content_type.contains("application/x-protobuf") {
        match parse_protobuf_logs(&body) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse protobuf logs: {}", e);
                return StatusCode::BAD_REQUEST;
            }
        }
    } else {
        // Default to JSON
        match parse_json_logs(&body) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse JSON logs: {}", e);
                return StatusCode::BAD_REQUEST;
            }
        }
    };

    if records.is_empty() {
        return StatusCode::OK;
    }

    debug!("Received {} log records", records.len());

    match state.storage.insert_logs(&records) {
        Ok(count) => {
            debug!("Stored {} log records", count);
            StatusCode::OK
        }
        Err(e) => {
            error!("Failed to store logs: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Parse OTLP JSON logs.
fn parse_json_logs(body: &[u8]) -> Result<Vec<LogRecord>, String> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;

    let mut records = Vec::new();
    let empty_vec: Vec<serde_json::Value> = vec![];

    let resource_logs = json
        .get("resourceLogs")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_vec);

    for resource_log in resource_logs {
        // Extract resource attributes
        let mut service_name = None;
        let mut app_path = None;
        let mut resource_attrs_json = None;

        if let Some(resource) = resource_log.get("resource") {
            if let Some(attrs) = resource.get("attributes").and_then(|v| v.as_array()) {
                let attrs_clone = attrs.clone();
                resource_attrs_json = Some(serde_json::to_string(&attrs_clone).unwrap_or_default());

                for attr in attrs {
                    let key = attr.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    let value = extract_any_value(attr.get("value"));

                    match key {
                        "service.name" => service_name = value,
                        "apx.app_path" => app_path = value,
                        _ => {}
                    }
                }
            }
        }

        // Extract log records from scope logs
        let empty_scope_logs: Vec<serde_json::Value> = vec![];
        let scope_logs = resource_log
            .get("scopeLogs")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_scope_logs);

        for scope_log in scope_logs {
            let empty_log_records: Vec<serde_json::Value> = vec![];
            let log_records = scope_log
                .get("logRecords")
                .and_then(|v| v.as_array())
                .unwrap_or(&empty_log_records);

            for record in log_records {
                let timestamp_ns = parse_timestamp(record.get("timeUnixNano"));
                let observed_timestamp_ns =
                    parse_timestamp(record.get("observedTimeUnixNano")).max(timestamp_ns);

                let severity_number = record
                    .get("severityNumber")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i32);

                let severity_text = record
                    .get("severityText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let body = extract_any_value(record.get("body"));

                let trace_id = record
                    .get("traceId")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "00000000000000000000000000000000")
                    .map(|s| s.to_string());

                let span_id = record
                    .get("spanId")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "0000000000000000")
                    .map(|s| s.to_string());

                let log_attrs = record
                    .get("attributes")
                    .map(|v| serde_json::to_string(v).unwrap_or_default());

                records.push(LogRecord {
                    timestamp_ns,
                    observed_timestamp_ns,
                    severity_number,
                    severity_text,
                    body,
                    service_name: service_name.clone(),
                    app_path: app_path.clone(),
                    resource_attributes: resource_attrs_json.clone(),
                    log_attributes: log_attrs,
                    trace_id,
                    span_id,
                });
            }
        }
    }

    Ok(records)
}

/// Parse OTLP Protobuf logs.
fn parse_protobuf_logs(body: &[u8]) -> Result<Vec<LogRecord>, String> {
    let request = ExportLogsServiceRequest::decode(body)
        .map_err(|e| format!("Failed to decode protobuf: {}", e))?;

    let mut records = Vec::new();

    for resource_log in request.resource_logs {
        // Extract resource attributes
        let mut service_name = None;
        let mut app_path = None;
        let mut resource_attrs_json = None;

        if let Some(resource) = &resource_log.resource {
            let attrs: Vec<serde_json::Value> = resource
                .attributes
                .iter()
                .map(|kv| {
                    serde_json::json!({
                        "key": kv.key,
                        "value": any_value_to_json(&kv.value)
                    })
                })
                .collect();

            resource_attrs_json = Some(serde_json::to_string(&attrs).unwrap_or_default());

            for kv in &resource.attributes {
                let value = kv.value.as_ref().and_then(|v| any_value_to_string(v));
                match kv.key.as_str() {
                    "service.name" => service_name = value,
                    "apx.app_path" => app_path = value,
                    _ => {}
                }
            }
        }

        for scope_log in resource_log.scope_logs {
            for record in scope_log.log_records {
                let timestamp_ns = record.time_unix_nano as i64;
                let observed_timestamp_ns =
                    (record.observed_time_unix_nano as i64).max(timestamp_ns);

                let severity_number = if record.severity_number != 0 {
                    Some(record.severity_number)
                } else {
                    None
                };

                let severity_text = if record.severity_text.is_empty() {
                    None
                } else {
                    Some(record.severity_text.clone())
                };

                let body = record.body.as_ref().and_then(|v| any_value_to_string(v));

                let trace_id = if record.trace_id.is_empty()
                    || record.trace_id.iter().all(|&b| b == 0)
                {
                    None
                } else {
                    Some(hex::encode(&record.trace_id))
                };

                let span_id =
                    if record.span_id.is_empty() || record.span_id.iter().all(|&b| b == 0) {
                        None
                    } else {
                        Some(hex::encode(&record.span_id))
                    };

                let log_attrs: Vec<serde_json::Value> = record
                    .attributes
                    .iter()
                    .map(|kv| {
                        serde_json::json!({
                            "key": kv.key,
                            "value": any_value_to_json(&kv.value)
                        })
                    })
                    .collect();
                let log_attrs_json = if log_attrs.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&log_attrs).unwrap_or_default())
                };

                records.push(LogRecord {
                    timestamp_ns,
                    observed_timestamp_ns,
                    severity_number,
                    severity_text,
                    body,
                    service_name: service_name.clone(),
                    app_path: app_path.clone(),
                    resource_attributes: resource_attrs_json.clone(),
                    log_attributes: log_attrs_json,
                    trace_id,
                    span_id,
                });
            }
        }
    }

    Ok(records)
}

/// Parse a timestamp from JSON (can be string or number).
fn parse_timestamp(value: Option<&serde_json::Value>) -> i64 {
    match value {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Extract a string value from an OTLP AnyValue JSON structure.
fn extract_any_value(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;

    // Try stringValue first
    if let Some(s) = v.get("stringValue").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }

    // Try intValue
    if let Some(n) = v.get("intValue") {
        if let Some(i) = n.as_i64() {
            return Some(i.to_string());
        }
        if let Some(s) = n.as_str() {
            return Some(s.to_string());
        }
    }

    // Try doubleValue
    if let Some(n) = v.get("doubleValue").and_then(|v| v.as_f64()) {
        return Some(n.to_string());
    }

    // Try boolValue
    if let Some(b) = v.get("boolValue").and_then(|v| v.as_bool()) {
        return Some(b.to_string());
    }

    // Fallback: serialize the whole value
    Some(serde_json::to_string(v).unwrap_or_default())
}

/// Convert protobuf AnyValue to a string.
fn any_value_to_string(
    value: &opentelemetry_proto::tonic::common::v1::AnyValue,
) -> Option<String> {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

    match &value.value {
        Some(Value::StringValue(s)) => Some(s.clone()),
        Some(Value::IntValue(i)) => Some(i.to_string()),
        Some(Value::DoubleValue(d)) => Some(d.to_string()),
        Some(Value::BoolValue(b)) => Some(b.to_string()),
        Some(Value::BytesValue(b)) => Some(hex::encode(b)),
        Some(Value::ArrayValue(arr)) => {
            let items: Vec<String> = arr
                .values
                .iter()
                .filter_map(|v| any_value_to_string(v))
                .collect();
            Some(format!("[{}]", items.join(", ")))
        }
        Some(Value::KvlistValue(kvlist)) => {
            let items: Vec<String> = kvlist
                .values
                .iter()
                .map(|kv| {
                    let val = kv
                        .value
                        .as_ref()
                        .and_then(|v| any_value_to_string(v))
                        .unwrap_or_default();
                    format!("{}={}", kv.key, val)
                })
                .collect();
            Some(format!("{{{}}}", items.join(", ")))
        }
        None => None,
    }
}

/// Convert protobuf AnyValue to JSON.
fn any_value_to_json(
    value: &Option<opentelemetry_proto::tonic::common::v1::AnyValue>,
) -> serde_json::Value {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

    let Some(value) = value else {
        return serde_json::Value::Null;
    };

    match &value.value {
        Some(Value::StringValue(s)) => serde_json::json!({ "stringValue": s }),
        Some(Value::IntValue(i)) => serde_json::json!({ "intValue": i }),
        Some(Value::DoubleValue(d)) => serde_json::json!({ "doubleValue": d }),
        Some(Value::BoolValue(b)) => serde_json::json!({ "boolValue": b }),
        Some(Value::BytesValue(b)) => serde_json::json!({ "bytesValue": hex::encode(b) }),
        Some(Value::ArrayValue(arr)) => {
            let items: Vec<serde_json::Value> = arr
                .values
                .iter()
                .map(|v| any_value_to_json(&Some(v.clone())))
                .collect();
            serde_json::json!({ "arrayValue": { "values": items } })
        }
        Some(Value::KvlistValue(kvlist)) => {
            let items: Vec<serde_json::Value> = kvlist
                .values
                .iter()
                .map(|kv| {
                    serde_json::json!({
                        "key": kv.key,
                        "value": any_value_to_json(&kv.value)
                    })
                })
                .collect();
            serde_json::json!({ "kvlistValue": { "values": items } })
        }
        None => serde_json::Value::Null,
    }
}
