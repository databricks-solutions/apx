use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex;
use tracing::field::Field;
use tracing::{Event, Subscriber};
use tracing_subscriber::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogStreamName {
    App,
    Ui,
    Apx,
    Db,
}

pub const APX_SHUTDOWN_MESSAGE: &str = "Dev server shutdown complete.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogPipe {
    Out,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPayload {
    pub stream: LogStreamName,
    pub pipe: Option<LogPipe>,
    pub message: String,
    #[serde(default)]
    pub timestamp: i64,
}

#[derive(Debug, Deserialize)]
pub struct BrowserLogPayload {
    pub level: String,
    pub source: String,
    pub message: String,
    pub stack: Option<String>,
    pub timestamp: i64,
}

impl LogPayload {
    pub fn new(stream: LogStreamName, pipe: Option<LogPipe>, message: String) -> Self {
        Self {
            stream,
            pipe,
            message,
            timestamp: Utc::now().timestamp_millis(),
        }
    }
}

/// Async-friendly log queue for subprocess logs (used by ProcessManager).
pub type LogQueue = Arc<Mutex<Vec<LogPayload>>>;

/// Sync log queue for tracing layer logs (used by ApxLogLayer).
pub type SyncLogQueue = Arc<StdMutex<Vec<LogPayload>>>;

static APX_LOG_QUEUE: OnceLock<SyncLogQueue> = OnceLock::new();

fn new_apx_log_queue() -> SyncLogQueue {
    Arc::new(StdMutex::new(Vec::new()))
}

pub fn apx_log_queue() -> SyncLogQueue {
    let queue = APX_LOG_QUEUE.get_or_init(new_apx_log_queue);
    Arc::clone(queue)
}

pub fn clear_apx_log_queue(queue: &SyncLogQueue) {
    let mut guard = queue.lock().expect("log queue poisoned");
    guard.clear();
}

fn push_apx_log(queue: &SyncLogQueue, payload: LogPayload) {
    let mut guard = queue.lock().expect("log queue poisoned");
    guard.push(payload);
}

pub fn apx_log_queue_since(queue: &SyncLogQueue, start_index: usize) -> (usize, Vec<LogPayload>) {
    let guard = queue.lock().expect("log queue poisoned");
    let len = guard.len();
    if start_index >= len {
        return (len, Vec::new());
    }
    let logs = guard[start_index..len].to_vec();
    (len, logs)
}

pub fn apx_log_queue_since_timestamp(
    queue: &SyncLogQueue,
    since: i64,
) -> (usize, Vec<LogPayload>) {
    let guard = queue.lock().expect("log queue poisoned");
    let len = guard.len();
    if since <= 0 {
        return (len, guard.clone());
    }
    let logs = guard
        .iter()
        .filter(|entry| entry.timestamp >= since)
        .cloned()
        .collect();
    (len, logs)
}

/// Async push for subprocess logs (ProcessManager).
pub async fn push_log(queue: &LogQueue, payload: LogPayload) {
    let mut guard = queue.lock().await;
    guard.push(payload);
}

/// Async read for subprocess logs (ProcessManager).
pub async fn log_queue_since(queue: &LogQueue, start_index: usize) -> (usize, Vec<LogPayload>) {
    let guard = queue.lock().await;
    let len = guard.len();
    if start_index >= len {
        return (len, Vec::new());
    }
    let logs = guard[start_index..len].to_vec();
    (len, logs)
}

/// Async read for subprocess logs (ProcessManager).
pub async fn log_queue_since_timestamp(queue: &LogQueue, since: i64) -> (usize, Vec<LogPayload>) {
    let guard = queue.lock().await;
    let len = guard.len();
    if since <= 0 {
        return (len, guard.clone());
    }
    let logs = guard
        .iter()
        .filter(|entry| entry.timestamp >= since)
        .cloned()
        .collect();
    (len, logs)
}

pub fn encode_log_payload(payload: &LogPayload) -> Result<String, String> {
    let bytes =
        rmp_serde::to_vec(payload).map_err(|err| format!("Failed to encode log payload: {err}"))?;
    Ok(BASE64_ENGINE.encode(bytes))
}

pub fn decode_log_payload(encoded: &str) -> Result<LogPayload, String> {
    let bytes = BASE64_ENGINE
        .decode(encoded)
        .map_err(|err| format!("Failed to decode log payload base64: {err}"))?;
    rmp_serde::from_slice(&bytes).map_err(|err| format!("Failed to decode log payload: {err}"))
}

pub struct ApxLogLayer;

impl<S> Layer<S> for ApxLogLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let queue = apx_log_queue();
        let message = format_event(event);
        let payload = LogPayload::new(LogStreamName::Apx, None, message);
        // Use synchronous push to ensure logs are captured immediately
        push_apx_log(&queue, payload);
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: Vec<String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.push(format!("{}={:?}", field.name(), value));
    }
}

fn format_event(event: &Event<'_>) -> String {
    let metadata = event.metadata();
    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);
    let target = metadata
        .target()
        .strip_prefix("_core::")
        .unwrap_or(metadata.target());
    if visitor.fields.is_empty() {
        return format!("{} {}", metadata.level(), target);
    }
    let (message, other_fields) = split_message_fields(&visitor.fields);
    match message {
        Some(message) if other_fields.is_empty() => {
            format!("{} {}: {}", metadata.level(), target, message)
        }
        Some(message) => {
            format!(
                "{} {}: {} {}",
                metadata.level(),
                target,
                message,
                other_fields.join(" ")
            )
        }
        None => format!("{} {} {}", metadata.level(), target, visitor.fields.join(" ")),
    }
}

fn split_message_fields(fields: &[String]) -> (Option<String>, Vec<String>) {
    let mut message = None;
    let mut others = Vec::new();
    for field in fields {
        if let Some(value) = field.strip_prefix("message=") {
            let cleaned = value.trim().trim_matches('"').to_string();
            message = Some(cleaned);
        } else {
            others.push(field.clone());
        }
    }
    (message, others)
}
