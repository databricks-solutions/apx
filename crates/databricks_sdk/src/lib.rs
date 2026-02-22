#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod client;
pub mod config;
pub mod config_parser;
pub mod error;
pub mod useragent;

pub use api::apps::{App, AppLogsArgs, ComputeState, LogEntry};
pub use api::current_user::{User, UserEmail, UserName};
pub use client::DatabricksClient;
pub use config::{DatabricksConfig, list_profile_names, resolve_config};
pub use config_parser::ConfigParser;
pub use error::{DatabricksError, Result};

/// Validate that the given Databricks profile has working credentials
/// by calling the SCIM /Me endpoint.
pub async fn validate_credentials(profile: &str) -> Result<()> {
    let client = DatabricksClient::new(profile).await?;
    client.current_user().me().await?;
    Ok(())
}

/// Get the forwarded user header value (`{user_id}@{workspace_id}`)
/// for proxying requests to Databricks-hosted apps.
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
