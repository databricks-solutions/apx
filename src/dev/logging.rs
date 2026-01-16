use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;
use tracing_subscriber::registry::LookupSpan;
use tracing::field::Field;
use tracing_subscriber::field::Visit;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStreamName {
    App,
    Ui,
    Apx,
}

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
}

pub type LogQueue = Arc<Mutex<VecDeque<LogPayload>>>;

static APX_LOG_QUEUE: OnceLock<LogQueue> = OnceLock::new();

pub fn new_log_queue() -> LogQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

pub fn set_apx_log_queue(queue: LogQueue) {
    let _ = APX_LOG_QUEUE.set(queue);
}

pub async fn drain_log_queue(queue: &LogQueue) -> Vec<LogPayload> {
    let mut guard = queue.lock().await;
    guard.drain(..).collect()
}

pub async fn is_log_queue_empty(queue: &LogQueue) -> bool {
    let guard = queue.lock().await;
    guard.is_empty()
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
        let queue = match APX_LOG_QUEUE.get() {
            Some(queue) => Arc::clone(queue),
            None => return,
        };
        let message = format_event(event);
        let payload = LogPayload {
            stream: LogStreamName::Apx,
            pipe: None,
            message,
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut guard = queue.lock().await;
                guard.push_back(payload);
            });
        }
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
