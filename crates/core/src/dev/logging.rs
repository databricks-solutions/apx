//! Logging types for browser log forwarding to flux.

use serde::Deserialize;

/// Browser log payload received from frontend via POST /_apx/logs
#[derive(Debug, Deserialize)]
pub struct BrowserLogPayload {
    pub level: String,
    pub source: String,
    pub message: String,
    pub stack: Option<String>,
    pub timestamp: i64,
}
