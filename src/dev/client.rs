use reqwest::blocking::Client;
use reqwest::blocking::Response;
use reqwest::StatusCode;
use std::time::Duration;
use tracing::{debug, warn};

use crate::dev::common::DEFAULT_HOST;

const DEFAULT_TIMEOUT_SECS: u64 = 2;

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

fn build_streaming_client() -> Result<Client, String> {
    debug!("Building streaming HTTP client (no timeout).");
    Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|err| {
            warn!(error = %err, "Failed to build streaming HTTP client.");
            format!("Failed to build HTTP client: {err}")
        })
}

fn build_url(host: &str, port: u16, path: &str) -> String {
    format!("http://{host}:{port}{path}")
}

pub fn health(port: u16) -> Result<bool, String> {
    let client = build_client()?;
    let url = build_url(DEFAULT_HOST, port, "/_apx/health");
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

pub fn logs(port: u16) -> Result<Response, String> {
    let client = build_streaming_client()?;
    let url = build_url(DEFAULT_HOST, port, "/_apx/logs");
    debug!(%url, "Opening dev server logs stream.");
    client
        .get(url)
        .header("Accept-Encoding", "identity")
        .send()
        .map_err(|err| {
            warn!(error = %err, "Logs request failed.");
            format!("Logs request failed: {err}")
        })
}
