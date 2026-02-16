use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{DatabricksError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksProfile {
    pub name: String,
    pub host: String,
}

#[derive(Debug, Clone)]
pub struct DatabricksConfig {
    pub profile: String,
    pub host: String,
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

/// Parse all profile sections and their host values from a databrickscfg file.
pub fn read_profiles(path: &Path) -> Result<Vec<DatabricksProfile>> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| DatabricksError::Config(format!("failed to read {}: {e}", path.display())))?;

    let mut profiles = Vec::new();
    let mut current_section: Option<String> = None;
    let mut current_host: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // Flush previous section
            if let Some(name) = current_section.take()
                && let Some(host) = current_host.take()
            {
                profiles.push(DatabricksProfile { name, host });
            }
            current_section = Some(trimmed[1..trimmed.len() - 1].to_string());
            current_host = None;
        } else if let Some((_key, value)) = trimmed.split_once('=') {
            let key = _key.trim();
            let value = value.trim();
            if key == "host" {
                current_host = Some(value.to_string());
            }
        }
    }
    // Flush last section
    if let Some(name) = current_section
        && let Some(host) = current_host
    {
        profiles.push(DatabricksProfile { name, host });
    }

    Ok(profiles)
}

/// List just the profile names (section headers) from `~/.databrickscfg`.
pub fn list_profile_names() -> Result<Vec<String>> {
    let path = config_file_path()?;
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| DatabricksError::Config(format!("failed to read {}: {e}", path.display())))?;

    let mut seen = HashSet::new();
    let mut profiles: Vec<String> = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let name = trimmed[1..trimmed.len() - 1].to_string();
                if seen.insert(name.clone()) {
                    Some(name)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    if seen.insert("DEFAULT".to_string()) {
        profiles.push("DEFAULT".to_string());
    }

    Ok(profiles)
}

/// Normalize a Databricks host URL: ensure https:// prefix and no trailing slash.
fn normalize_host(host: &str) -> String {
    let mut h = host.to_string();
    if !h.starts_with("https://") && !h.starts_with("http://") {
        h = format!("https://{h}");
    }
    h.trim_end_matches('/').to_string()
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
    let profiles = read_profiles(&path)?;

    let found = profiles.iter().find(|p| p.name == profile).ok_or_else(|| {
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

    Ok(DatabricksConfig { profile, host })
}
