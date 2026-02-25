//! Databricks SDK for Rust.
//!
//! Provides authenticated access to the Databricks REST API via the Databricks CLI
//! for token management. Configuration is read from `~/.databrickscfg`.

/// Databricks REST API wrappers (apps, current user, etc.).
pub mod api;
/// Token acquisition and caching via the Databricks CLI.
pub mod auth;
/// HTTP client with automatic token refresh.
pub mod client;
/// Configuration resolution from `~/.databrickscfg`.
pub mod config;
/// INI-style parser for `.databrickscfg` files.
pub mod config_parser;
/// Error types and the crate-level `Result` alias.
pub mod error;
/// User-Agent header builder matching the Databricks SDK format.
pub mod useragent;

pub use api::apps::{App, AppLogsArgs, ComputeState, LogEntry};
pub use api::current_user::{User, UserEmail, UserName};
pub use client::DatabricksClient;
pub use config::{DatabricksConfig, list_profile_names, resolve_config};
pub use config_parser::ConfigParser;
pub use error::{DatabricksError, Result};

/// Validate that the given Databricks profile has working credentials
/// by calling the SCIM /Me endpoint.
///
/// # Errors
///
/// Returns an error if the profile cannot be resolved or the authentication check fails.
pub async fn validate_credentials(profile: &str) -> Result<()> {
    let client = DatabricksClient::new(profile).await?;
    client.current_user().me().await?;
    Ok(())
}

/// Get the forwarded user header value (`{user_id}@{workspace_id}`)
/// for proxying requests to Databricks-hosted apps.
///
/// # Errors
///
/// Returns an error if the profile cannot be resolved or the current user cannot be fetched.
pub async fn get_forwarded_user_header(profile: &str) -> Result<String> {
    let client = DatabricksClient::new(profile).await?;
    let user = client.current_user().me().await?;
    let workspace_id = extract_workspace_id(client.host());
    Ok(format!("{}@{}", user.id, workspace_id))
}

/// Extract the workspace ID from a Databricks host URL.
///
/// E.g. `https://adb-1234567890123456.7.azuredatabricks.net`
///   → split on `-` → `["https://adb", "1234567890123456.7.azuredatabricks.net"]`
///   → index 1 → split on `.` → `"1234567890123456"`
fn extract_workspace_id(host: &str) -> String {
    host.split('-')
        .nth(1)
        .and_then(|segment| segment.split('.').next())
        .unwrap_or("placeholder")
        .to_string()
}
