use std::collections::HashSet;
use std::path::Path;

use crate::error::{DatabricksError, Result};

/// A parsed `.databrickscfg` INI file.
///
/// Extracts section names and `host` values — the only fields we need.
#[derive(Debug, Clone)]
pub struct ConfigParser {
    profiles: Vec<ProfileEntry>,
}

/// A single `[section]` with its `host = ...` value.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    /// Section name (e.g. `"DEFAULT"`, `"staging"`).
    pub name: String,
    /// The `host` value from this section.
    pub host: String,
}

impl ConfigParser {
    /// Parse a `.databrickscfg` file at `path`.
    /// Returns an empty config if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read.
    pub fn parse(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                profiles: Vec::new(),
            });
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            DatabricksError::Config(format!("failed to read {}: {e}", path.display()))
        })?;

        Ok(Self::parse_str(&content))
    }

    /// Parse from an in-memory string (useful for testing).
    #[must_use]
    pub fn parse_str(content: &str) -> Self {
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
                    profiles.push(ProfileEntry { name, host });
                }
                current_section = Some(trimmed[1..trimmed.len() - 1].to_string());
                current_host = None;
            } else if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
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
            profiles.push(ProfileEntry { name, host });
        }

        Self { profiles }
    }

    /// All parsed profiles with their host values.
    #[must_use]
    pub fn profiles(&self) -> &[ProfileEntry] {
        &self.profiles
    }

    /// Find a profile by name.
    #[must_use]
    pub fn get_profile(&self, name: &str) -> Option<&ProfileEntry> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// List unique profile names (section headers), plus `DEFAULT` if not already present.
    #[must_use]
    pub fn list_profiles(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut names: Vec<String> = self
            .profiles
            .iter()
            .filter_map(|p| {
                if seen.insert(p.name.clone()) {
                    Some(p.name.clone())
                } else {
                    None
                }
            })
            .collect();

        if seen.insert("DEFAULT".to_string()) {
            names.push("DEFAULT".to_string());
        }

        names
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let config = ConfigParser::parse_str("");
        assert!(config.profiles().is_empty());
        assert_eq!(config.list_profiles(), vec!["DEFAULT"]);
    }

    #[test]
    fn test_parse_single_profile() {
        let content = "\
[my-profile]
host = https://my-workspace.cloud.databricks.com
token = dapiXXX
";
        let config = ConfigParser::parse_str(content);
        assert_eq!(config.profiles().len(), 1);
        assert_eq!(config.profiles()[0].name, "my-profile");
        assert_eq!(
            config.profiles()[0].host,
            "https://my-workspace.cloud.databricks.com"
        );
    }

    #[test]
    fn test_parse_multiple_profiles() {
        let content = "\
[DEFAULT]
host = https://default.cloud.databricks.com

[staging]
host = https://staging.cloud.databricks.com

[production]
host = https://production.cloud.databricks.com
";
        let config = ConfigParser::parse_str(content);
        assert_eq!(config.profiles().len(), 3);
        assert_eq!(
            config.list_profiles(),
            vec!["DEFAULT", "staging", "production"]
        );
    }

    #[test]
    fn test_get_profile() {
        let content = "\
[DEFAULT]
host = https://default.cloud.databricks.com

[staging]
host = https://staging.cloud.databricks.com
";
        let config = ConfigParser::parse_str(content);
        let staging = config
            .get_profile("staging")
            .expect("staging profile not found");
        assert_eq!(staging.host, "https://staging.cloud.databricks.com");
        assert!(config.get_profile("nonexistent").is_none());
    }

    #[test]
    fn test_section_without_host_is_skipped() {
        let content = "\
[no-host]
token = dapiXXX

[has-host]
host = https://example.com
";
        let config = ConfigParser::parse_str(content);
        assert_eq!(config.profiles().len(), 1);
        assert_eq!(config.profiles()[0].name, "has-host");
    }
}
