use std::path::PathBuf;

use crate::config_parser::ConfigParser;
use crate::error::{DatabricksError, Result};

#[derive(Debug, Clone)]
pub struct DatabricksConfig {
    pub profile: String,
    pub host: String,
    pub product: Option<String>,
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
            "profile '{}' has no host configured",
            profile
        )));
    }

    Ok(DatabricksConfig {
        profile,
        host,
        product: None,
        product_version: None,
    })
}
