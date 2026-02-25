//! HTTP client for communicating with APX dev server.

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::{debug, warn};

use apx_common::hosts::CLIENT_HOST;

use crate::dev::token::DEV_TOKEN_HEADER;

const DEFAULT_TIMEOUT_SECS: u64 = 5;
const STOP_TIMEOUT_SECS: u64 = 10;

/// Shared HTTP client for dev server communication.
/// Reused across health(), status(), and stop() to avoid creating a new client per call.
static DEV_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

/// Default timeout for health checks (in seconds)
const HEALTH_TIMEOUT_SECS: u64 = 60;
/// Delay between health check retries (in ms)
const HEALTH_RETRY_DELAY_MS: u64 = 200;
/// Initial delay before starting health checks (give server time to start)
const HEALTH_INITIAL_DELAY_MS: u64 = 1000;

/// Distinguishes connection-level failures from server-level errors in health checks.
#[derive(Debug)]
pub enum HealthError {
    /// Server is not reachable (connection refused, timeout, no service on port)
    ConnectionFailed(String),
    /// Server is up but responded with an error (non-OK HTTP, bad JSON, etc.)
    ServerError(String),
}

impl std::fmt::Display for HealthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(msg) | Self::ServerError(msg) => write!(f, "{msg}"),
        }
    }
}

/// Configuration for health check waiting behavior
#[derive(Debug, Clone, Copy)]
pub struct HealthCheckConfig {
    /// Total timeout for health checks (in seconds)
    pub timeout_secs: u64,
    /// Delay between health check retries (in ms)
    pub retry_delay_ms: u64,
    /// Initial delay before starting health checks (in ms)
    pub initial_delay_ms: u64,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            timeout_secs: HEALTH_TIMEOUT_SECS,
            retry_delay_ms: HEALTH_RETRY_DELAY_MS,
            initial_delay_ms: HEALTH_INITIAL_DELAY_MS,
        }
    }
}

/// Response from the dev server status endpoint.
#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    /// Overall server status.
    pub status: String,
    /// Frontend process status.
    pub frontend_status: String,
    /// Backend process status.
    pub backend_status: String,
    /// Embedded database status.
    pub db_status: String,
    /// True if any critical process (frontend/backend) has permanently failed and cannot recover.
    pub failed: bool,
}

fn build_url(host: &str, port: u16, path: &str) -> String {
    format!("http://{host}:{port}{path}")
}

/// Check if the dev server at the given port is healthy.
pub async fn health(port: u16) -> Result<bool, String> {
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server health request.");
    let response = DEV_CLIENT
        .get(&url)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|err| {
            // Use debug! not warn! since health check failures are expected during startup
            debug!(error = %err, %url, "Health request failed (server may still be starting).");
            format!("Health request failed: {err}")
        })?;
    let ok = response.status() == StatusCode::OK;
    debug!(status = %response.status(), ok, "Received dev server health response.");
    Ok(ok)
}

/// Get the status of the dev server including frontend and backend statuses.
pub async fn status(port: u16) -> Result<StatusResponse, HealthError> {
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server status request.");
    let response = DEV_CLIENT
        .get(&url)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|err| {
            debug!(error = %err, %url, "Status request failed to connect.");
            HealthError::ConnectionFailed(format!("connection failed: {err}"))
        })?;

    let http_status = response.status();
    debug!(%url, status = %http_status, "Received HTTP response for status request.");

    if http_status != StatusCode::OK {
        let body = response.text().await.unwrap_or_default();
        let body_preview = if body.len() > 500 {
            &body[..500]
        } else {
            &body
        };
        warn!(
            %url, status = %http_status, body = %body_preview,
            "Health endpoint returned non-OK status."
        );
        return Err(HealthError::ServerError(format!(
            "HTTP {http_status}: {body_preview}"
        )));
    }

    // Get response body as text first for debugging
    let body_text = response.text().await.map_err(|err| {
        warn!(error = %err, %url, "Failed to read status response body.");
        HealthError::ServerError(format!("failed to read response body: {err}"))
    })?;

    debug!(%url, body = %body_text, "Status response body received.");

    let status_response: StatusResponse = serde_json::from_str(&body_text).map_err(|err| {
        warn!(error = %err, %url, body = %body_text, "Failed to parse status response JSON.");
        HealthError::ServerError(format!("invalid JSON response: {err} (body: {body_text})"))
    })?;

    debug!(
        %url,
        status = %status_response.status,
        frontend_status = %status_response.frontend_status,
        backend_status = %status_response.backend_status,
        db_status = %status_response.db_status,
        "Parsed status response successfully."
    );
    Ok(status_response)
}

/// Request the dev server to stop gracefully.
/// Returns Ok(()) if the server acknowledged the stop request, Err otherwise.
pub async fn stop(port: u16, token: Option<&str>) -> Result<(), String> {
    let url = build_url(CLIENT_HOST, port, "/_apx/stop");
    debug!(%url, "Sending dev server stop request.");
    let mut request = DEV_CLIENT
        .get(&url)
        .timeout(Duration::from_secs(STOP_TIMEOUT_SECS));
    if let Some(t) = token {
        request = request.header(DEV_TOKEN_HEADER, t);
    }
    let response = request.send().await.map_err(|err| {
        warn!(error = %err, "Stop request failed.");
        format!("Stop request failed: {err}")
    })?;
    if response.status() == StatusCode::OK {
        debug!("Dev server stop request acknowledged.");
        Ok(())
    } else {
        warn!(status = %response.status(), "Dev server stop request failed.");
        Err(format!(
            "Stop request failed with status {}",
            response.status()
        ))
    }
}
