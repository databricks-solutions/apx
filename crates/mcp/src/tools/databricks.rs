use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use apx_core::dotenv::DotenvFile;
use apx_databricks_sdk::{AppLogsArgs, DatabricksClient, LogEntry};
use rmcp::model::*;
use rmcp::schemars;
use serde_json::Value;

use crate::server::ApxServer;
use crate::tools::{ToolError, ToolResultExt};
use crate::validation::validated_app_path;

pub(crate) fn resolve_app_name_from_databricks_yml(project_dir: &Path) -> Result<String, String> {
    let yml_path = project_dir.join("databricks.yml");
    if !yml_path.exists() {
        return Err(format!(
            "Could not auto-detect app name because databricks.yml was not found at {}. \
            Please pass app_name explicitly.",
            yml_path.display()
        ));
    }

    let contents = std::fs::read_to_string(&yml_path)
        .map_err(|e| format!("Failed to read databricks.yml: {e}"))?;

    let data: Value = serde_yaml::from_str(&contents)
        .map_err(|e| format!("Failed to parse databricks.yml: {e}"))?;

    let apps_obj = extract_apps_object(&data)?;
    let app_names = collect_app_names(apps_obj);

    match app_names.len() {
        1 => Ok(app_names[0].clone()),
        0 => Err(
            "Could not auto-detect app name because no apps were found in databricks.yml under \
            resources.apps.*.name. Please pass app_name explicitly."
                .to_string(),
        ),
        _ => Err(format!(
            "Could not auto-detect app name because multiple apps were found in databricks.yml \
            ({}). Please pass app_name explicitly.",
            app_names.join(", ")
        )),
    }
}

fn extract_apps_object(data: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    data.get("resources")
        .ok_or_else(|| "databricks.yml 'resources' must be a mapping/object".to_string())?
        .get("apps")
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())?
        .as_object()
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())
}

fn collect_app_names(apps_obj: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut names = HashSet::new();
    for app_def in apps_obj.values() {
        if let Some(app_obj) = app_def.as_object()
            && let Some(name_val) = app_obj.get("name")
            && let Some(name_str) = name_val.as_str()
        {
            let name = name_str.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    let mut sorted: Vec<String> = names.into_iter().collect();
    sorted.sort();
    sorted
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DatabricksAppsLogsArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Name of the Databricks app (auto-detected from databricks.yml if not provided)
    #[serde(default)]
    pub app_name: Option<String>,
    /// Number of tail lines to fetch (default: 200)
    #[serde(default = "default_tail_lines")]
    pub tail_lines: u32,
    /// Search string to filter logs
    #[serde(default)]
    pub search: Option<String>,
    /// Log sources to include (e.g. ["APP"], ["SYSTEM"], or ["APP", "SYSTEM"])
    #[serde(default)]
    pub source: Option<Vec<String>>,
    /// Databricks CLI profile
    #[serde(default)]
    pub profile: Option<String>,
    /// Timeout in seconds (default: 60)
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: f64,
}

fn default_tail_lines() -> u32 {
    200
}

fn default_timeout_seconds() -> f64 {
    60.0
}

impl ApxServer {
    pub async fn handle_databricks_apps_logs(
        &self,
        args: DatabricksAppsLogsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cwd = validated_app_path(&args.app_path)?;

        // Load env vars from .env if present
        let dotenv_vars = load_dotenv_vars(&cwd);

        // Resolve app_name
        let resolved = match resolve_app_name(&args, &cwd) {
            Ok(r) => r,
            Err(e) => return ToolError::OperationFailed(e).into_result(),
        };

        // Resolve profile: explicit arg → .env DATABRICKS_CONFIG_PROFILE → "" (SDK default)
        let profile = resolve_profile(&args, &dotenv_vars);

        let client = match get_or_create_client(&self.ctx.databricks_clients, &profile).await {
            Ok(c) => c,
            Err(e) => {
                return ToolError::OperationFailed(format!(
                    "Failed to create Databricks client: {e}"
                ))
                .into_result();
            }
        };

        let start = std::time::Instant::now();
        let logs_args = AppLogsArgs {
            app_name: &resolved.name,
            tail_lines: args.tail_lines as usize,
            search: args.search.as_deref(),
            sources: args.source.as_deref(),
            timeout: Duration::from_secs_f64(args.timeout_seconds),
            idle_timeout: None,
        };

        let entries = match client.apps().logs(&logs_args).await {
            Ok(e) => e,
            Err(e) => {
                return ToolError::OperationFailed(format!("Failed to fetch app logs: {e}"))
                    .into_result();
            }
        };

        let duration_ms = start.elapsed().as_millis() as i64;

        tool_response! {
            struct DatabricksAppsLogsResponse {
                app_name: String,
                resolved_from_databricks_yml: bool,
                log_count: usize,
                entries: Vec<LogEntry>,
                duration_ms: i64,
            }
        }

        let response = DatabricksAppsLogsResponse {
            app_name: resolved.name,
            resolved_from_databricks_yml: resolved.from_yml,
            log_count: entries.len(),
            entries,
            duration_ms,
        };

        Ok(CallToolResult::from_serializable(&response))
    }
}

fn load_dotenv_vars(cwd: &Path) -> HashMap<String, String> {
    let dotenv_path = cwd.join(".env");
    if dotenv_path.exists() {
        DotenvFile::read(&dotenv_path)
            .map(|dotenv| dotenv.get_vars())
            .unwrap_or_default()
    } else {
        HashMap::new()
    }
}

struct ResolvedAppName {
    name: String,
    from_yml: bool,
}

fn resolve_app_name(
    args: &DatabricksAppsLogsArgs,
    cwd: &Path,
) -> std::result::Result<ResolvedAppName, String> {
    match args.app_name.as_ref() {
        Some(name) if !name.trim().is_empty() => Ok(ResolvedAppName {
            name: name.trim().to_string(),
            from_yml: false,
        }),
        _ => match resolve_app_name_from_databricks_yml(cwd) {
            Ok(name) => Ok(ResolvedAppName {
                name,
                from_yml: true,
            }),
            Err(e) => Err(format!("Failed to auto-detect app name: {e}")),
        },
    }
}

async fn get_or_create_client(
    cache: &tokio::sync::RwLock<HashMap<String, DatabricksClient>>,
    profile: &str,
) -> std::result::Result<DatabricksClient, apx_databricks_sdk::DatabricksError> {
    // Fast path: read lock
    {
        let clients = cache.read().await;
        if let Some(client) = clients.get(profile) {
            return Ok(client.clone());
        }
    }

    // Slow path: write lock with double-check
    let mut clients = cache.write().await;
    if let Some(client) = clients.get(profile) {
        return Ok(client.clone());
    }

    let client = DatabricksClient::new(profile).await?;
    clients.insert(profile.to_string(), client.clone());
    Ok(client)
}

fn resolve_profile(args: &DatabricksAppsLogsArgs, dotenv_vars: &HashMap<String, String>) -> String {
    if let Some(ref p) = args.profile {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Some(p) = dotenv_vars.get("DATABRICKS_CONFIG_PROFILE") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    String::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolve_app_name_from_databricks_yml_basic() {
        let dir = std::env::temp_dir().join("apx_test_resolve_app");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let yml_content = r#"
resources:
  apps:
    my_app:
      name: my-cool-app
      source_code_path: ./src
"#;
        std::fs::write(dir.join("databricks.yml"), yml_content).unwrap();

        let result = resolve_app_name_from_databricks_yml(&dir);
        assert_eq!(result.unwrap(), "my-cool-app");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_app_name_multiple_apps_returns_error() {
        let dir = std::env::temp_dir().join("apx_test_resolve_multi");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let yml_content = r#"
resources:
  apps:
    app1:
      name: first-app
    app2:
      name: second-app
"#;
        std::fs::write(dir.join("databricks.yml"), yml_content).unwrap();

        let result = resolve_app_name_from_databricks_yml(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("multiple apps"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_app_name_no_file_returns_error() {
        let dir = std::env::temp_dir().join("apx_test_resolve_nofile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = resolve_app_name_from_databricks_yml(&dir);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_profile_explicit_arg() {
        let args = DatabricksAppsLogsArgs {
            app_path: "/tmp".to_string(),
            app_name: None,
            tail_lines: 200,
            search: None,
            source: None,
            profile: Some("my-profile".to_string()),
            timeout_seconds: 60.0,
        };
        let dotenv = HashMap::new();
        assert_eq!(resolve_profile(&args, &dotenv), "my-profile");
    }

    #[test]
    fn resolve_profile_from_dotenv() {
        let args = DatabricksAppsLogsArgs {
            app_path: "/tmp".to_string(),
            app_name: None,
            tail_lines: 200,
            search: None,
            source: None,
            profile: None,
            timeout_seconds: 60.0,
        };
        let mut dotenv = HashMap::new();
        dotenv.insert(
            "DATABRICKS_CONFIG_PROFILE".to_string(),
            "env-profile".to_string(),
        );
        assert_eq!(resolve_profile(&args, &dotenv), "env-profile");
    }

    #[test]
    fn resolve_profile_default_empty() {
        let args = DatabricksAppsLogsArgs {
            app_path: "/tmp".to_string(),
            app_name: None,
            tail_lines: 200,
            search: None,
            source: None,
            profile: None,
            timeout_seconds: 60.0,
        };
        let dotenv = HashMap::new();
        assert_eq!(resolve_profile(&args, &dotenv), "");
    }
}
