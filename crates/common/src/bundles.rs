//! Databricks bundle configuration parsing and app name resolution.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;

/// Filename for Databricks bundle configuration.
const DATABRICKS_YML: &str = "databricks.yml";

/// Parsed representation of a `databricks.yml` configuration file.
#[derive(Debug, Deserialize)]
pub struct BundleConfig {
    /// Top-level resources section of the bundle.
    #[serde(default)]
    pub resources: BundleResources,
}

/// Resources section of a Databricks bundle configuration.
#[derive(Debug, Default, Deserialize)]
pub struct BundleResources {
    /// Map of app resource keys to their definitions.
    #[serde(default)]
    pub apps: HashMap<String, AppResource>,
}

/// A single Databricks App resource definition.
#[derive(Debug, Deserialize)]
pub struct AppResource {
    /// Display name of the app.
    pub name: String,
}

impl BundleConfig {
    /// # Errors
    ///
    /// Returns an error if `databricks.yml` is missing or cannot be parsed.
    pub fn from_path(dir: &Path) -> Result<Self, String> {
        let yml_path = dir.join(DATABRICKS_YML);
        if !yml_path.exists() {
            return Err(format!(
                "databricks.yml not found at {}",
                yml_path.display()
            ));
        }

        let contents = std::fs::read_to_string(&yml_path)
            .map_err(|e| format!("Failed to read databricks.yml: {e}"))?;

        Self::from_yaml(&contents)
    }

    /// # Errors
    ///
    /// Returns an error if the YAML content cannot be parsed.
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        serde_yaml::from_str(yaml).map_err(|e| format!("Failed to parse databricks.yml: {e}"))
    }

    /// Return sorted, deduplicated app names from the bundle configuration.
    pub fn app_names(&self) -> Vec<String> {
        let names: BTreeSet<&str> = self
            .resources
            .apps
            .values()
            .map(|app| app.name.trim())
            .filter(|n| !n.is_empty())
            .collect();
        names.into_iter().map(String::from).collect()
    }
}

/// # Errors
///
/// Returns an error if zero or more than one app is defined in `databricks.yml`.
pub fn resolve_single_app_name(project_dir: &Path) -> Result<String, String> {
    let config = BundleConfig::from_path(project_dir)?;
    let names = config.app_names();

    match names.len() {
        1 => Ok(names.into_iter().next().unwrap_or_default()),
        0 => Err(
            "Could not auto-detect app name because no apps were found in databricks.yml under \
             resources.apps.*.name. Please pass app_name explicitly."
                .to_string(),
        ),
        _ => Err(format!(
            "Could not auto-detect app name because multiple apps were found in databricks.yml \
             ({}). Please pass app_name explicitly.",
            names.join(", ")
        )),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_raw_string_hashes
)]
mod tests {
    use super::*;

    // --- BundleConfig::from_yaml ---

    #[test]
    fn parse_single_app() {
        let yaml = r"
resources:
  apps:
    my_app:
      name: my-cool-app
      source_code_path: ./src
";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.resources.apps.len(), 1);
        assert_eq!(config.resources.apps["my_app"].name, "my-cool-app");
    }

    #[test]
    fn parse_missing_resources_section() {
        let yaml = "bundle:\n  name: test\n";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert!(config.resources.apps.is_empty());
    }

    #[test]
    fn parse_invalid_yaml() {
        let yaml = ":\n  :\n  [invalid";
        assert!(BundleConfig::from_yaml(yaml).is_err());
    }

    #[test]
    fn parse_extra_unknown_fields() {
        let yaml = r#"
bundle:
  name: test
  cluster_id: abc
resources:
  apps:
    my_app:
      name: my-app
      source_code_path: ./src
      description: "an app"
  pipelines:
    my_pipeline:
      name: p1
"#;
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.resources.apps.len(), 1);
        assert_eq!(config.resources.apps["my_app"].name, "my-app");
    }

    #[test]
    fn parse_empty_apps_map() {
        let yaml = "resources:\n  apps: {}\n";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert!(config.resources.apps.is_empty());
    }

    // --- BundleConfig::app_names ---

    #[test]
    fn app_names_single() {
        let yaml = r"
resources:
  apps:
    app1:
      name: alpha
";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.app_names(), vec!["alpha"]);
    }

    #[test]
    fn app_names_multiple_sorted() {
        let yaml = r"
resources:
  apps:
    z_app:
      name: zulu
    a_app:
      name: alpha
";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.app_names(), vec!["alpha", "zulu"]);
    }

    #[test]
    fn app_names_deduplicates() {
        let yaml = r"
resources:
  apps:
    app1:
      name: same-name
    app2:
      name: same-name
";
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.app_names(), vec!["same-name"]);
    }

    #[test]
    fn app_names_filters_whitespace() {
        let yaml = r#"
resources:
  apps:
    app1:
      name: "  "
    app2:
      name: real-app
"#;
        let config = BundleConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.app_names(), vec!["real-app"]);
    }

    // --- resolve_single_app_name (file-based) ---

    #[test]
    fn resolve_single_app_from_file() {
        let dir = std::env::temp_dir().join("apx_test_bundles_single");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let yaml = "resources:\n  apps:\n    a:\n      name: my-app\n";
        std::fs::write(dir.join(DATABRICKS_YML), yaml).unwrap();

        let result = resolve_single_app_name(&dir);
        assert_eq!(result.unwrap(), "my-app");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_multiple_apps_errors() {
        let dir = std::env::temp_dir().join("apx_test_bundles_multi");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let yaml = "resources:\n  apps:\n    a:\n      name: app1\n    b:\n      name: app2\n";
        std::fs::write(dir.join(DATABRICKS_YML), yaml).unwrap();

        let result = resolve_single_app_name(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("multiple apps"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_no_apps_errors() {
        let dir = std::env::temp_dir().join("apx_test_bundles_none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let yaml = "resources:\n  apps: {}\n";
        std::fs::write(dir.join(DATABRICKS_YML), yaml).unwrap();

        let result = resolve_single_app_name(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no apps"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_missing_file_errors() {
        let dir = std::env::temp_dir().join("apx_test_bundles_nofile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = resolve_single_app_name(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
