use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tracing::debug;

use crate::error::{DatabricksError, Result};

/// JSON response from `databricks auth token`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CliTokenResponse {
    pub access_token: String,
    #[allow(dead_code)]
    pub token_type: String,
    pub expiry: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedToken {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
}

/// Staleness buffer: consider token expired 40s before actual expiry.
const STALENESS_BUFFER_SECS: i64 = 40;

impl CachedToken {
    pub fn is_valid(&self) -> bool {
        let buffer = Duration::seconds(STALENESS_BUFFER_SECS);
        Utc::now() + buffer < self.expires_at
    }
}

/// Acquire a fresh token by shelling out to the Databricks CLI.
pub(crate) async fn acquire_token(profile: &str) -> Result<CachedToken> {
    debug!(profile, "Acquiring token via Databricks CLI");

    let output = tokio::process::Command::new("databricks")
        .args(["auth", "token", "--profile", profile])
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DatabricksError::Cli(
                    "Databricks CLI not found. Install it: https://docs.databricks.com/dev-tools/cli/install.html".to_string(),
                )
            } else {
                DatabricksError::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DatabricksError::Auth(format!(
            "databricks auth token failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }

    let response: CliTokenResponse = serde_json::from_slice(&output.stdout)?;

    let expires_at = DateTime::parse_from_rfc3339(&response.expiry)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            DatabricksError::Auth(format!(
                "failed to parse token expiry '{}': {e}",
                response.expiry
            ))
        })?;

    debug!(profile, %expires_at, "Token acquired successfully");

    Ok(CachedToken {
        access_token: response.access_token,
        expires_at,
    })
}
