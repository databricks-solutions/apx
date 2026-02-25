//! Logging types for browser log forwarding to flux.

use serde::Deserialize;

/// Browser log payload received from frontend via POST /_apx/logs
#[derive(Debug, Deserialize)]
pub struct BrowserLogPayload {
    /// Log level (e.g. `"error"`, `"warn"`, `"info"`).
    pub level: String,
    /// Log source identifier (e.g. `"console"`, `"onerror"`).
    pub source: String,
    /// The log message text.
    pub message: String,
    /// Optional JavaScript stack trace.
    pub stack: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}
