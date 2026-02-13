//! HTTP client for communicating with APX dev server.

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json;
use std::time::Duration;
use tracing::{debug, warn};

use crate::dev::common::CLIENT_HOST;

const DEFAULT_TIMEOUT_SECS: u64 = 2;
const STOP_TIMEOUT_SECS: u64 = 10;

/// Default timeout for health checks (in seconds)
const HEALTH_TIMEOUT_SECS: u64 = 60;
/// Delay between health check retries (in ms)
const HEALTH_RETRY_DELAY_MS: u64 = 200;
/// Initial delay before starting health checks (give server time to start)
const HEALTH_INITIAL_DELAY_MS: u64 = 1000;

/// Configuration for health check waiting behavior
#[derive(Debug, Clone)]
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

/// Wait for the dev server to become healthy.
/// Returns Ok(()) if healthy, Err with message if timeout exceeded.
#[allow(dead_code)]
pub async fn wait_for_healthy(port: u16, config: &HealthCheckConfig) -> Result<(), String> {
    use std::time::Instant;

    // Give server time to start Python/tokio before polling
    tokio::time::sleep(Duration::from_millis(config.initial_delay_ms)).await;

    let deadline = Instant::now() + Duration::from_secs(config.timeout_secs);
    let mut first_attempt = true;

    while Instant::now() < deadline {
        match status(port).await {
            Ok(status_response) if status_response.status == "ok" => return Ok(()),
            Ok(status_response) => {
                // Log which services aren't ready yet (only on first attempt)
                if first_attempt {
                    debug!(
                        "Services not ready - frontend: {}, backend: {}, db: {}",
                        status_response.frontend_status,
                        status_response.backend_status,
                        status_response.db_status
                    );
                    first_attempt = false;
                }
                tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)).await;
            }
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)).await;
            }
        }
    }

    Err(format!(
        "Dev server failed to become healthy after {}s timeout",
        config.timeout_secs
    ))
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub frontend_status: String,
    pub backend_status: String,
    pub db_status: String,
    /// True if any critical process (frontend/backend) has permanently failed and cannot recover
    pub failed: bool,
}

fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|err| {
            warn!(error = %err, "Failed to build dev HTTP client.");
            format!("Failed to build HTTP client: {err}")
        })
}

fn build_url(host: &str, port: u16, path: &str) -> String {
    format!("http://{host}:{port}{path}")
}

pub async fn health(port: u16) -> Result<bool, String> {
    let client = build_client()?;
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server health request.");
    let response = client
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
pub async fn status(port: u16) -> Result<StatusResponse, String> {
    let client = build_client()?;
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server status request.");
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|err| {
            debug!(error = %err, %url, "Status request failed to connect.");
            format!("Status request failed: {err}")
        })?;

    let http_status = response.status();
    debug!(%url, status = %http_status, "Received HTTP response for status request.");

    if http_status != StatusCode::OK {
        return Err(format!("Status request failed with status {http_status}"));
    }

    // Get response body as text first for debugging
    let body_text = response.text().await.map_err(|err| {
        warn!(error = %err, %url, "Failed to read status response body.");
        format!("Failed to read status response body: {err}")
    })?;

    debug!(%url, body = %body_text, "Status response body received.");

    let status_response: StatusResponse = serde_json::from_str(&body_text).map_err(|err| {
        warn!(error = %err, %url, body = %body_text, "Failed to parse status response JSON.");
        format!("Failed to parse status response: {err}")
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
pub async fn stop(port: u16) -> Result<(), String> {
    let client = build_client()?;
    let url = build_url(CLIENT_HOST, port, "/_apx/stop");
    debug!(%url, "Sending dev server stop request.");
    let response = client
        .get(url)
        .timeout(Duration::from_secs(STOP_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|err| {
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
