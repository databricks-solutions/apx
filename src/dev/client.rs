use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, warn};

use crate::dev::common::CLIENT_HOST;

const DEFAULT_TIMEOUT_SECS: u64 = 2;
const STOP_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub frontend_status: String,
    pub backend_status: String,
}

fn build_async_client() -> Result<reqwest::Client, String> {
    debug!("Building async HTTP client.");
    reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|err| {
            warn!(error = %err, "Failed to build async HTTP client.");
            format!("Failed to build HTTP client: {err}")
        })
}

fn build_client() -> Result<Client, String> {
    debug!("Building dev HTTP client.");
    Client::builder()
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|err| {
            warn!(error = %err, "Failed to build dev HTTP client.");
            format!("Failed to build HTTP client: {err}")
        })
}

fn build_client_with_timeout(timeout_secs: u64) -> Result<Client, String> {
    debug!("Building dev HTTP client with {timeout_secs}s timeout.");
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
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

pub fn health(port: u16) -> Result<bool, String> {
    let client = build_client()?;
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server health request.");
    let response = client
        .get(url)
        .send()
        .map_err(|err| {
            warn!(error = %err, "Health request failed.");
            format!("Health request failed: {err}")
        })?;
    let ok = response.status() == StatusCode::OK;
    debug!(status = %response.status(), ok, "Received dev server health response.");
    Ok(ok)
}

/// Get the status of the dev server including frontend and backend statuses.
pub fn status(port: u16) -> Result<StatusResponse, String> {
    let client = build_client()?;
    let url = build_url(CLIENT_HOST, port, "/_apx/health");
    debug!(%url, "Sending dev server status request.");
    let response = client
        .get(url)
        .send()
        .map_err(|err| {
            warn!(error = %err, "Status request failed.");
            format!("Status request failed: {err}")
        })?;
    
    if response.status() != StatusCode::OK {
        return Err(format!(
            "Status request failed with status {}",
            response.status()
        ));
    }
    
    let status_response: StatusResponse = response.json().map_err(|err| {
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
pub fn stop(port: u16) -> Result<(), String> {
    let client = build_client_with_timeout(STOP_TIMEOUT_SECS)?;
    let url = build_url(CLIENT_HOST, port, "/_apx/stop");
    debug!(%url, "Sending dev server stop request.");
    let response = client.get(url).send().map_err(|err| {
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

pub async fn logs_async(
    port: u16,
    since: Option<i64>,
    follow: bool,
) -> Result<reqwest::Response, String> {
    let client = build_async_client()?;
    let url = build_logs_url(port, since, follow);
    debug!(%url, "Opening async dev server logs stream.");
    client
        .get(url)
        .header("Accept-Encoding", "identity")
        .send()
        .await
        .map_err(|err| {
            warn!(error = %err, "Async logs request failed.");
            format!("Logs request failed: {err}")
        })
}

fn build_logs_url(port: u16, since: Option<i64>, follow: bool) -> String {
    let mut url = build_url(CLIENT_HOST, port, "/_apx/logs");
    let mut params: Vec<String> = Vec::new();
    if let Some(since) = since {
        params.push(format!("since={since}"));
    }
    if follow {
        params.push("follow=true".to_string());
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}
