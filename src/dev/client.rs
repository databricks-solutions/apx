//! HTTP client for communicating with APX dev server.

use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

use crate::dev::common::CLIENT_HOST;

const DEFAULT_TIMEOUT_SECS: u64 = 2;
const STOP_TIMEOUT_SECS: u64 = 10;

/// Default number of health check retries
const HEALTH_RETRY_COUNT: u32 = 50;
/// Delay between health check retries (in ms)
const HEALTH_RETRY_DELAY_MS: u64 = 200;
/// Initial delay before starting health checks (give server time to start)
const HEALTH_INITIAL_DELAY_MS: u64 = 1000;

/// Configuration for health check waiting behavior
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    pub retry_count: u32,
    pub retry_delay_ms: u64,
    pub initial_delay_ms: u64,
    pub print_waiting: bool,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            retry_count: HEALTH_RETRY_COUNT,
            retry_delay_ms: HEALTH_RETRY_DELAY_MS,
            initial_delay_ms: HEALTH_INITIAL_DELAY_MS,
            print_waiting: true,
        }
    }
}

/// Wait for the dev server to become healthy.
/// Returns Ok(()) if healthy, Err with message if not healthy after retries.
pub async fn wait_for_healthy(port: u16, config: &HealthCheckConfig) -> Result<(), String> {
    // Give server time to start Python/tokio before polling
    tokio::time::sleep(Duration::from_millis(config.initial_delay_ms)).await;

    for attempt in 0..config.retry_count {
        match status(port).await {
            Ok(status_response) if status_response.status == "ok" => return Ok(()),
            Ok(status_response) => {
                // Log which services aren't ready yet (only on first attempt and if debugging)
                if attempt == 0 {
                    debug!(
                        "Services not ready - frontend: {}, backend: {}, db: {}",
                        status_response.frontend_status, 
                        status_response.backend_status,
                        status_response.db_status
                    );
                    if config.print_waiting {
                        println!("Waiting for dev server to become healthy...");
                    }
                }
                tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)).await;
            }
            Err(_) => {
                // Only print status on first attempt
                if attempt == 0 && config.print_waiting {
                    println!("Waiting for dev server to become healthy...");
                }
                tokio::time::sleep(Duration::from_millis(config.retry_delay_ms)).await;
            }
        }
    }

    Err(format!(
        "Dev server failed to become healthy after {} retries",
        config.retry_count
    ))
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub frontend_status: String,
    pub backend_status: String,
    pub db_status: String,
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
        .get(url)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|err| {
            debug!(error = %err, "Status request failed.");
            format!("Status request failed: {err}")
        })?;
    
    if response.status() != StatusCode::OK {
        return Err(format!(
            "Status request failed with status {}",
            response.status()
        ));
    }
    
    let status_response: StatusResponse = response.json().await.map_err(|err| {
        warn!(error = %err, "Failed to parse status response.");
        format!("Failed to parse status response: {err}")
    })?;
    
    debug!(
        frontend_status = %status_response.frontend_status,
        backend_status = %status_response.backend_status,
        "Received dev server status response."
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
