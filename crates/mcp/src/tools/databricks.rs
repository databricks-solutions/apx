use crate::server::ApxServer;
use crate::tools::ToolResultExt;
use crate::validation::validate_app_path;
use apx_core::dotenv::DotenvFile;
use rmcp::model::*;
use rmcp::schemars;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

pub(crate) fn truncate(s: &str, max_chars: i32) -> String {
    if max_chars <= 0 {
        return String::new();
    }
    let max_chars = max_chars as usize;
    if s.len() <= max_chars {
        return s.to_string();
    }
    let head_len = max_chars.saturating_sub(50);
    let tail_len = if max_chars >= 100 { 40 } else { 0 };
    let head = &s[..head_len];
    let tail = if tail_len > 0 {
        &s[s.len().saturating_sub(tail_len)..]
    } else {
        ""
    };
    let truncated = s.len() - head_len - tail_len;
    format!("{head}\n\n...[truncated {truncated} chars]...\n\n{tail}")
}

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

    let resources = data
        .get("resources")
        .ok_or_else(|| "databricks.yml 'resources' must be a mapping/object".to_string())?;

    let apps = resources
        .get("apps")
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())?;

    let apps_obj = apps
        .as_object()
        .ok_or_else(|| "databricks.yml 'resources.apps' must be a mapping/object".to_string())?;

    let mut app_names = HashSet::new();
    for app_def in apps_obj.values() {
        if let Some(app_obj) = app_def.as_object()
            && let Some(name_val) = app_obj.get("name")
            && let Some(name_str) = name_val.as_str()
        {
            let name = name_str.trim();
            if !name.is_empty() {
                app_names.insert(name.to_string());
            }
        }
    }

    let mut app_names_vec: Vec<String> = app_names.into_iter().collect();
    app_names_vec.sort();

    match app_names_vec.len() {
        1 => Ok(app_names_vec[0].clone()),
        0 => Err(
            "Could not auto-detect app name because no apps were found in databricks.yml under \
            resources.apps.*.name. Please pass app_name explicitly."
                .to_string(),
        ),
        _ => Err(format!(
            "Could not auto-detect app name because multiple apps were found in databricks.yml \
            ({}). Please pass app_name explicitly.",
            app_names_vec.join(", ")
        )),
    }
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
    pub tail_lines: i32,
    /// Search string to filter logs
    #[serde(default)]
    pub search: Option<String>,
    /// Log sources to include
    #[serde(default)]
    pub source: Option<Vec<String>>,
    /// Databricks CLI profile
    #[serde(default)]
    pub profile: Option<String>,
    /// Databricks CLI target
    #[serde(default)]
    pub target: Option<String>,
    /// Output format (default: "text")
    #[serde(default = "default_output")]
    pub output: String,
    /// Timeout in seconds (default: 60)
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: f64,
    /// Maximum output characters (default: 20000)
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: i32,
}

fn default_tail_lines() -> i32 {
    200
}

fn default_output() -> String {
    "text".to_string()
}

fn default_timeout_seconds() -> f64 {
    60.0
}

fn default_max_output_chars() -> i32 {
    20000
}

impl ApxServer {
    pub async fn handle_databricks_apps_logs(
        &self,
        args: DatabricksAppsLogsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let cwd = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        let mut resolved_from_yml = false;

        // Load env vars from .env if present
        let dotenv_path = cwd.join(".env");
        let dotenv_vars: HashMap<String, String> = if dotenv_path.exists() {
            DotenvFile::read(&dotenv_path)
                .map(|dotenv| dotenv.get_vars())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };

        // Resolve app_name if not provided
        let app_name = match args.app_name.as_ref() {
            Some(name) if !name.trim().is_empty() => name.trim().to_string(),
            _ => match resolve_app_name_from_databricks_yml(&cwd) {
                Ok(name) => {
                    resolved_from_yml = true;
                    name
                }
                Err(e) => {
                    return Ok(CallToolResult::error(vec![Content::text(format!(
                        "Failed to auto-detect app name: {e}"
                    ))]));
                }
            },
        };

        // Build command and track arguments for response
        let mut cmd_args = vec!["apps".to_string(), "logs".to_string(), app_name.clone()];
        let mut cmd = Command::new("databricks");
        cmd.args(&cmd_args)
            .arg("--tail-lines")
            .arg(args.tail_lines.to_string())
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd_args.push("--tail-lines".to_string());
        cmd_args.push(args.tail_lines.to_string());

        let mut push_flag_value = |flag: &str, value: Option<&str>| {
            if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
                cmd.arg(flag).arg(value);
                cmd_args.push(flag.to_string());
                cmd_args.push(value.to_string());
            }
        };

        push_flag_value("--search", args.search.as_deref());
        push_flag_value("-p", args.profile.as_deref());
        push_flag_value("-t", args.target.as_deref());

        if let Some(sources) = &args.source {
            for src in sources {
                cmd.arg("--source").arg(src);
                cmd_args.push("--source".to_string());
                cmd_args.push(src.clone());
            }
        }

        cmd.arg("-o").arg(&args.output);
        cmd_args.push("-o".to_string());
        cmd_args.push(args.output.clone());

        if !dotenv_vars.is_empty() {
            cmd.envs(&dotenv_vars);
        }

        let mut full_command = vec!["databricks".to_string()];
        full_command.extend(cmd_args.clone());
        let cmd_str = full_command.join(" ");

        // Run command with timeout
        let start = Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs_f64(args.timeout_seconds), cmd.output()).await;

        let (returncode, stdout, stderr, duration_ms) = match result {
            Ok(Ok(cmd_output)) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                let returncode = cmd_output.status.code().unwrap_or(0);
                let stdout = String::from_utf8_lossy(&cmd_output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&cmd_output.stderr).to_string();
                (returncode, stdout, stderr, duration_ms)
            }
            Ok(Err(e)) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(CallToolResult::error(vec![Content::text(
                        "Databricks CLI executable not found (`databricks`). \
                        Please install Databricks CLI v0.280.0 or higher and ensure it's on PATH.",
                    )]));
                }
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to execute command: {e}"
                ))]));
            }
            Err(_) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Timed out after {}s running: {}",
                    args.timeout_seconds, cmd_str
                ))]));
            }
        };

        let stdout_t = truncate(&stdout, args.max_output_chars);
        let stderr_t = truncate(&stderr, args.max_output_chars);

        if returncode != 0 {
            let combined = format!("{stderr}\n{stdout}").to_lowercase();
            if combined.contains("unknown command \"logs\"")
                || combined.contains("unknown command logs")
                || combined.contains("unknown subcommand")
                || combined.contains("no such command")
            {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Databricks CLI does not support `databricks apps logs` in this version. \
                    Please upgrade Databricks CLI to v0.280.0 or higher.\n\n\
                    Command: {cmd_str}\n\
                    Exit code: {returncode}\n\
                    stderr:\n{stderr_t}\n\
                    stdout:\n{stdout_t}"
                ))]));
            }

            return Ok(CallToolResult::error(vec![Content::text(format!(
                "`databricks apps logs` failed.\n\n\
                Command: {cmd_str}\n\
                Exit code: {returncode}\n\
                stderr:\n{stderr_t}\n\
                stdout:\n{stdout_t}"
            ))]));
        }

        #[derive(Serialize)]
        struct DatabricksAppsLogsResponse {
            app_name: String,
            resolved_from_databricks_yml: bool,
            command: Vec<String>,
            cwd: String,
            returncode: i32,
            stdout: String,
            stderr: String,
            duration_ms: i64,
        }

        let response = DatabricksAppsLogsResponse {
            app_name,
            resolved_from_databricks_yml: resolved_from_yml,
            command: full_command,
            cwd: cwd.to_string_lossy().to_string(),
            returncode,
            stdout: stdout_t,
            stderr: stderr_t,
            duration_ms,
        };

        Ok(CallToolResult::from_serializable(&response))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 100), "");
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_zero_max() {
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn truncate_negative_max() {
        assert_eq!(truncate("hello", -1), "");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(1000);
        let result = truncate(&long, 200);
        assert!(result.contains("truncated"));
        assert!(result.len() < 1000);
    }

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
}
