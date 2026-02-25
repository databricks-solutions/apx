use std::path::PathBuf;

use crate::config_parser::ConfigParser;
use crate::error::{DatabricksError, Result};

/// Resolved Databricks workspace configuration for a single profile.
#[derive(Debug, Clone)]
pub struct DatabricksConfig {
    /// Profile name (e.g. `"DEFAULT"`, `"staging"`).
    pub profile: String,
    /// Normalized workspace host URL (e.g. `"https://adb-123.4.azuredatabricks.net"`).
    pub host: String,
    /// Optional product name for the User-Agent header.
    pub product: Option<String>,
    /// Optional product version for the User-Agent header.
    pub product_version: Option<String>,
}

/// Return the path to the Databricks config file.
/// Respects `DATABRICKS_CONFIG_FILE` env var, defaults to `~/.databrickscfg`.
fn config_file_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("DATABRICKS_CONFIG_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| DatabricksError::Config("could not determine home directory".to_string()))?;
    Ok(home.join(".databrickscfg"))
}

/// Normalize a Databricks host URL: ensure https:// prefix and no trailing slash.
fn normalize_host(host: &str) -> String {
    let mut h = host.to_string();
    if !h.starts_with("https://") && !h.starts_with("http://") {
        h = format!("https://{h}");
    }
    h.trim_end_matches('/').to_string()
}

/// List just the profile names (section headers) from `~/.databrickscfg`.
///
/// # Errors
///
/// Returns an error if the config file path cannot be determined or the file cannot be parsed.
pub fn list_profile_names() -> Result<Vec<String>> {
    let path = config_file_path()?;
    let config = ConfigParser::parse(&path)?;
    Ok(config.list_profiles())
}

/// Resolve a full `DatabricksConfig` for the given profile name.
///
/// Profile resolution order:
/// 1. Explicit `profile_name` argument (if non-empty)
/// 2. `DATABRICKS_CONFIG_PROFILE` env var
/// 3. `"DEFAULT"`
///
/// # Errors
///
/// Returns an error if the config file cannot be read, the profile is not found,
/// or the profile has no host configured.
pub fn resolve_config(profile_name: &str) -> Result<DatabricksConfig> {
    let profile = if !profile_name.is_empty() {
        profile_name.to_string()
    } else if let Ok(env_profile) = std::env::var("DATABRICKS_CONFIG_PROFILE") {
        env_profile
    } else {
        "DEFAULT".to_string()
    };

    let path = config_file_path()?;
    let config = ConfigParser::parse(&path)?;

    let found = config.get_profile(&profile).ok_or_else(|| {
        DatabricksError::Config(format!(
            "profile '{}' not found in {}",
            profile,
            path.display()
        ))
    })?;

    let host = normalize_host(&found.host);
    if host.is_empty() {
        return Err(DatabricksError::Config(format!(
            "profile '{profile}' has no host configured"
        )));
    }

    Ok(DatabricksConfig {
        profile,
        host,
        product: None,
        product_version: None,
    })
}
