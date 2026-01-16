use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;
use tracing_subscriber::registry::LookupSpan;
use tracing::field::Field;
use tracing_subscriber::field::Visit;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogStreamName {
    App,
    Ui,
    Apx,
}

pub const APX_SHUTDOWN_MESSAGE: &str = "Dev server shutdown complete.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

pub type LogQueue = Arc<Mutex<Vec<LogPayload>>>;

static APX_LOG_QUEUE: OnceLock<LogQueue> = OnceLock::new();

fn new_log_queue() -> LogQueue {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn apx_log_queue() -> LogQueue {
    let queue = APX_LOG_QUEUE.get_or_init(new_log_queue);
    Arc::clone(queue)
}

pub async fn clear_log_queue(queue: &LogQueue) {
    let mut guard = queue.lock().await;
    guard.clear();
}

pub async fn push_log(queue: &LogQueue, payload: LogPayload) {
    let mut guard = queue.lock().await;
    guard.push(payload);
}

pub async fn log_queue_since(queue: &LogQueue, start_index: usize) -> (usize, Vec<LogPayload>) {
    let guard = queue.lock().await;
    let len = guard.len();
    if start_index >= len {
        return (len, Vec::new());
    }
    let logs = guard[start_index..len].to_vec();
    (len, logs)
}

pub async fn log_queue_since_timestamp(
    queue: &LogQueue,
    since: i64,
) -> (usize, Vec<LogPayload>) {
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
        if !matches!(
            *event.metadata().level(),
            Level::DEBUG | Level::INFO | Level::WARN | Level::ERROR
        ) {
            return;
        }
        let queue = apx_log_queue();
        let message = format_event(event);
        let payload = LogPayload::new(LogStreamName::Apx, None, message);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                push_log(&queue, payload).await;
            });
            return;
        }

        // Fall back when no runtime is available (e.g. spawn_blocking or sync thread).
        let mut guard = queue.blocking_lock();
        guard.push(payload);
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
    if visitor.fields.is_empty() {
        format!("{} {}", metadata.level(), metadata.target())
    } else {
        format!(
            "{} {} {}",
            metadata.level(),
            metadata.target(),
            visitor.fields.join(" ")
        )
    }
}
