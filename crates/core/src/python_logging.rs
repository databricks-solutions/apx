//! Python logging configuration for uvicorn.
//!
//! Supports two configuration modes:
//! 1. Inline TOML config in [tool.apx.dev.logging]
//! 2. External Python file via log_config_file setting
//!
//! When neither is specified, generates a default logging configuration.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Dev configuration from [tool.apx.dev]
#[derive(Debug, Clone, Default)]
pub struct DevConfig {
    /// Inline TOML logging config
    pub logging: Option<LoggingConfig>,
    /// External Python file for logging config
    pub log_config_file: Option<PathBuf>,
}

/// Python logging.dictConfig format
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub version: i32,
    #[serde(default)]
    pub disable_existing_loggers: bool,
    #[serde(default)]
    pub formatters: HashMap<String, FormatterConfig>,
    #[serde(default)]
    pub handlers: HashMap<String, HandlerConfig>,
    #[serde(default)]
    pub loggers: HashMap<String, LoggerConfig>,
    #[serde(default)]
    pub root: Option<RootLoggerConfig>,
}

/// Formatter configuration
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatterConfig {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub datefmt: Option<String>,
    #[serde(default, rename = "class")]
    pub class_name: Option<String>,
}

/// Handler configuration
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerConfig {
    #[serde(rename = "class")]
    pub class_name: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub formatter: Option<String>,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub filters: Option<Vec<String>>,
}

/// Logger configuration
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerConfig {
    #[serde(default)]
    pub handlers: Option<Vec<String>>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub propagate: Option<bool>,
}

/// Root logger configuration
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootLoggerConfig {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub handlers: Option<Vec<String>>,
}

/// Result of resolving log configuration
#[derive(Debug, Clone)]
pub enum LogConfigResult {
    /// JSON config file path (.apx/uvicorn_logging.json)
    JsonConfig(PathBuf),
    /// External Python file path
    PythonFile(PathBuf),
}

impl LogConfigResult {
    /// Get the path as a string for passing to uvicorn
    pub fn to_string_path(&self) -> String {
        match self {
            LogConfigResult::JsonConfig(p) | LogConfigResult::PythonFile(p) => {
                p.display().to_string()
            }
        }
    }
}

/// Parse [tool.apx.dev] section from pyproject.toml
pub fn parse_dev_config(
    pyproject_value: &toml::Value,
    project_root: &Path,
) -> Result<DevConfig, String> {
    let dev_section = pyproject_value
        .get("tool")
        .and_then(|tool| tool.get("apx"))
        .and_then(|apx| apx.get("dev"));

    let Some(dev) = dev_section else {
        return Ok(DevConfig::default());
    };

    let logging = dev.get("logging").map(parse_logging_config).transpose()?;

    let log_config_file = dev
        .get("log_config_file")
        .and_then(|v| v.as_str())
        .map(|s| project_root.join(s));

    // Validate mutual exclusivity
    if logging.is_some() && log_config_file.is_some() {
        return Err(
            "Cannot specify both [tool.apx.dev.logging] and log_config_file in pyproject.toml"
                .to_string(),
        );
    }

    // Validate external file exists
    if let Some(ref path) = log_config_file
        && !path.exists()
    {
        return Err(format!("log_config_file not found: {}", path.display()));
    }

    Ok(DevConfig {
        logging,
        log_config_file,
    })
}

/// Parse inline logging configuration from TOML value
fn parse_logging_config(value: &toml::Value) -> Result<LoggingConfig, String> {
    let version = value
        .get("version")
        .and_then(|v| v.as_integer())
        .unwrap_or(1) as i32;

    let disable_existing_loggers = value
        .get("disable_existing_loggers")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let formatters = parse_formatters(value.get("formatters"))?;
    let handlers = parse_handlers(value.get("handlers"))?;
    let loggers = parse_loggers(value.get("loggers"))?;
    let root = parse_root_logger(value.get("root"))?;

    Ok(LoggingConfig {
        version,
        disable_existing_loggers,
        formatters,
        handlers,
        loggers,
        root,
    })
}

fn parse_formatters(
    value: Option<&toml::Value>,
) -> Result<HashMap<String, FormatterConfig>, String> {
    let Some(v) = value else {
        return Ok(HashMap::new());
    };

    let table = v.as_table().ok_or("formatters must be a table")?;

    let mut result = HashMap::new();
    for (name, formatter_value) in table {
        let formatter_table = formatter_value
            .as_table()
            .ok_or_else(|| format!("formatter '{}' must be a table", name))?;

        let format = formatter_table
            .get("format")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let datefmt = formatter_table
            .get("datefmt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let class_name = formatter_table
            .get("class")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        result.insert(
            name.clone(),
            FormatterConfig {
                format,
                datefmt,
                class_name,
            },
        );
    }

    Ok(result)
}

fn parse_handlers(value: Option<&toml::Value>) -> Result<HashMap<String, HandlerConfig>, String> {
    let Some(v) = value else {
        return Ok(HashMap::new());
    };

    let table = v.as_table().ok_or("handlers must be a table")?;

    let mut result = HashMap::new();
    for (name, handler_value) in table {
        let handler_table = handler_value
            .as_table()
            .ok_or_else(|| format!("handler '{}' must be a table", name))?;

        let class_name = handler_table
            .get("class")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("handler '{}' must have a 'class' field", name))?;

        let level = handler_table
            .get("level")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let formatter = handler_table
            .get("formatter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let stream = handler_table
            .get("stream")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let filename = handler_table
            .get("filename")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let filters = handler_table.get("filters").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });

        result.insert(
            name.clone(),
            HandlerConfig {
                class_name,
                level,
                formatter,
                stream,
                filename,
                filters,
            },
        );
    }

    Ok(result)
}

fn parse_loggers(value: Option<&toml::Value>) -> Result<HashMap<String, LoggerConfig>, String> {
    let Some(v) = value else {
        return Ok(HashMap::new());
    };

    let table = v.as_table().ok_or("loggers must be a table")?;

    let mut result = HashMap::new();
    for (name, logger_value) in table {
        let logger_table = logger_value
            .as_table()
            .ok_or_else(|| format!("logger '{}' must be a table", name))?;

        let handlers = logger_table.get("handlers").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });

        let level = logger_table
            .get("level")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let propagate = logger_table.get("propagate").and_then(|v| v.as_bool());

        result.insert(
            name.clone(),
            LoggerConfig {
                handlers,
                level,
                propagate,
            },
        );
    }

    Ok(result)
}

fn parse_root_logger(value: Option<&toml::Value>) -> Result<Option<RootLoggerConfig>, String> {
    let Some(v) = value else {
        return Ok(None);
    };

    let table = v.as_table().ok_or("root must be a table")?;

    let level = table
        .get("level")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let handlers = table.get("handlers").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    Ok(Some(RootLoggerConfig { level, handlers }))
}

/// Generate the default logging configuration for uvicorn
pub fn default_logging_config(app_slug: &str) -> LoggingConfig {
    let mut formatters = HashMap::new();
    formatters.insert(
        "default".to_string(),
        FormatterConfig {
            format: Some("%(levelname)-7s / %(name)-30.30s / %(message)s".to_string()),
            datefmt: None,
            class_name: None,
        },
    );
    formatters.insert(
        "access".to_string(),
        FormatterConfig {
            format: Some("%(message)s".to_string()),
            datefmt: None,
            class_name: None,
        },
    );

    let mut handlers = HashMap::new();
    handlers.insert(
        "default".to_string(),
        HandlerConfig {
            class_name: "logging.StreamHandler".to_string(),
            level: None,
            formatter: Some("default".to_string()),
            stream: Some("ext://sys.stderr".to_string()),
            filename: None,
            filters: None,
        },
    );
    handlers.insert(
        "access".to_string(),
        HandlerConfig {
            class_name: "logging.StreamHandler".to_string(),
            level: None,
            formatter: Some("access".to_string()),
            stream: Some("ext://sys.stdout".to_string()),
            filename: None,
            filters: None,
        },
    );

    let mut loggers = HashMap::new();
    loggers.insert(
        "uvicorn".to_string(),
        LoggerConfig {
            handlers: Some(vec!["default".to_string()]),
            level: Some("INFO".to_string()),
            propagate: Some(false),
        },
    );
    loggers.insert(
        "uvicorn.error".to_string(),
        LoggerConfig {
            handlers: None,
            level: Some("INFO".to_string()),
            propagate: Some(true),
        },
    );
    loggers.insert(
        "uvicorn.access".to_string(),
        LoggerConfig {
            handlers: Some(vec!["access".to_string()]),
            level: Some("INFO".to_string()),
            propagate: Some(false),
        },
    );
    // App-specific logger at DEBUG level
    loggers.insert(
        app_slug.to_string(),
        LoggerConfig {
            handlers: Some(vec!["default".to_string()]),
            level: Some("DEBUG".to_string()),
            propagate: Some(false),
        },
    );
    // Silence Databricks SDK internal logging (auth probes, config loading, etc.)
    loggers.insert(
        "databricks.sdk".to_string(),
        LoggerConfig {
            handlers: None,
            level: Some("INFO".to_string()),
            propagate: Some(true),
        },
    );

    let root = Some(RootLoggerConfig {
        level: Some("INFO".to_string()),
        handlers: Some(vec!["default".to_string()]),
    });

    LoggingConfig {
        version: 1,
        disable_existing_loggers: false,
        formatters,
        handlers,
        loggers,
        root,
    }
}

/// Write logging configuration to JSON file in .apx directory
pub async fn write_logging_config_json(
    config: &LoggingConfig,
    app_dir: &Path,
) -> Result<PathBuf, String> {
    let config_dir = app_dir.join(".apx");
    tokio::fs::create_dir_all(&config_dir)
        .await
        .map_err(|e| format!("Failed to create .apx directory: {e}"))?;

    let config_path = config_dir.join("uvicorn_logging.json");

    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize logging config: {e}"))?;

    tokio::fs::write(&config_path, json)
        .await
        .map_err(|e| format!("Failed to write uvicorn logging config: {e}"))?;

    Ok(config_path)
}

/// Merge user-provided logging config with the default config.
///
/// User-provided formatters, handlers, and loggers are merged into the defaults,
/// with user values taking precedence over defaults for any overlapping keys.
pub fn merge_with_default(user_config: &LoggingConfig, app_slug: &str) -> LoggingConfig {
    let mut config = default_logging_config(app_slug);

    // Merge formatters (user overrides defaults)
    for (name, formatter) in &user_config.formatters {
        config.formatters.insert(name.clone(), formatter.clone());
    }

    // Merge handlers (user overrides defaults)
    for (name, handler) in &user_config.handlers {
        config.handlers.insert(name.clone(), handler.clone());
    }

    // Merge loggers (user overrides defaults)
    for (name, logger) in &user_config.loggers {
        config.loggers.insert(name.clone(), logger.clone());
    }

    // Override root if user provided one
    if user_config.root.is_some() {
        config.root = user_config.root.clone();
    }

    // Use user's disable_existing_loggers if they explicitly set it
    // (We can't distinguish "not set" from "set to false" in TOML, but
    // since false is the sensible default for uvicorn, this is fine)
    config.disable_existing_loggers = user_config.disable_existing_loggers;

    config
}

/// Resolve the logging configuration to use for uvicorn
///
/// Priority:
/// 1. External Python file (log_config_file)
/// 2. Inline TOML config merged with defaults ([tool.apx.dev.logging])
/// 3. Default config
pub async fn resolve_log_config(
    dev_config: &DevConfig,
    app_slug: &str,
    app_dir: &Path,
) -> Result<LogConfigResult, String> {
    // External Python file takes precedence
    if let Some(ref py_file) = dev_config.log_config_file {
        return Ok(LogConfigResult::PythonFile(py_file.clone()));
    }

    // Merge inline config with defaults, or use defaults alone
    let config = match &dev_config.logging {
        Some(user_cfg) => merge_with_default(user_cfg, app_slug),
        None => default_logging_config(app_slug),
    };

    let json_path = write_logging_config_json(&config, app_dir).await?;
    Ok(LogConfigResult::JsonConfig(json_path))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_default_logging_config() {
        let config = default_logging_config("myapp");

        assert_eq!(config.version, 1);
        assert!(!config.disable_existing_loggers);
        assert!(config.formatters.contains_key("default"));
        assert!(config.formatters.contains_key("access"));
        assert!(config.handlers.contains_key("default"));
        assert!(config.handlers.contains_key("access"));
        assert!(config.loggers.contains_key("uvicorn"));
        assert!(config.loggers.contains_key("uvicorn.error"));
        assert!(config.loggers.contains_key("uvicorn.access"));
        assert!(config.loggers.contains_key("myapp"));
        assert!(config.loggers.contains_key("databricks.sdk"));

        // App logger should be at DEBUG level
        if let Some(app_logger) = config.loggers.get("myapp") {
            assert_eq!(app_logger.level, Some("DEBUG".to_string()));
        } else {
            panic!("myapp logger should exist");
        }

        // Databricks SDK logger should be at INFO level
        if let Some(sdk_logger) = config.loggers.get("databricks.sdk") {
            assert_eq!(sdk_logger.level, Some("INFO".to_string()));
        } else {
            panic!("databricks.sdk logger should exist");
        }
    }

    #[test]
    fn test_parse_dev_config_empty() {
        let toml_str = r#"
[tool.apx.metadata]
app-name = "Test"
"#;
        let Ok(value) = toml_str.parse::<toml::Value>() else {
            panic!("failed to parse TOML");
        };
        let Ok(config) = parse_dev_config(&value, Path::new("/test")) else {
            panic!("failed to parse dev config");
        };

        assert!(config.logging.is_none());
        assert!(config.log_config_file.is_none());
    }

    #[test]
    fn test_parse_dev_config_inline_logging() {
        let toml_str = r#"
[tool.apx.dev.logging]
version = 1
disable_existing_loggers = true

[tool.apx.dev.logging.formatters.custom]
format = "%(levelname)s - %(message)s"

[tool.apx.dev.logging.handlers.console]
class = "logging.StreamHandler"
stream = "ext://sys.stdout"
formatter = "custom"

[tool.apx.dev.logging.loggers.mylogger]
handlers = ["console"]
level = "DEBUG"
propagate = false
"#;
        let Ok(value) = toml_str.parse::<toml::Value>() else {
            panic!("failed to parse TOML");
        };
        let Ok(config) = parse_dev_config(&value, Path::new("/test")) else {
            panic!("failed to parse dev config");
        };

        assert!(config.logging.is_some());
        assert!(config.log_config_file.is_none());

        let Some(logging) = config.logging else {
            panic!("logging config should exist");
        };
        assert_eq!(logging.version, 1);
        assert!(logging.disable_existing_loggers);
        assert!(logging.formatters.contains_key("custom"));
        assert!(logging.handlers.contains_key("console"));
        assert!(logging.loggers.contains_key("mylogger"));

        if let Some(mylogger) = logging.loggers.get("mylogger") {
            assert_eq!(mylogger.level, Some("DEBUG".to_string()));
            assert_eq!(mylogger.propagate, Some(false));
        } else {
            panic!("mylogger should exist");
        }
    }

    #[test]
    fn test_parse_dev_config_mutual_exclusivity() {
        let toml_str = r#"
[tool.apx.dev]
log_config_file = "logging.py"

[tool.apx.dev.logging]
version = 1
"#;
        let Ok(value) = toml_str.parse::<toml::Value>() else {
            panic!("failed to parse TOML");
        };
        // Create a temp file to simulate the external config existing
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("logging.py");
        if std::fs::write(&temp_file, "# logging config").is_err() {
            panic!("failed to write temp file");
        }

        let result = parse_dev_config(&value, &temp_dir);
        assert!(result.is_err());
        if let Err(err) = result {
            assert!(err.contains("Cannot specify both"));
        }

        std::fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_logging_config_serialization() {
        let config = default_logging_config("testapp");
        let Ok(json) = serde_json::to_string_pretty(&config) else {
            panic!("failed to serialize");
        };

        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"disable_existing_loggers\": false"));
        assert!(json.contains("\"testapp\""));
        assert!(json.contains("\"DEBUG\""));
    }

    #[test]
    fn test_log_config_result_to_string() {
        let json_result = LogConfigResult::JsonConfig(PathBuf::from("/app/.apx/logging.json"));
        assert_eq!(json_result.to_string_path(), "/app/.apx/logging.json");

        let py_result = LogConfigResult::PythonFile(PathBuf::from("/app/logging_config.py"));
        assert_eq!(py_result.to_string_path(), "/app/logging_config.py");
    }

    #[test]
    fn test_merge_with_default_preserves_defaults() {
        // User config with just one custom logger
        let mut user_config = LoggingConfig {
            version: 1,
            disable_existing_loggers: false,
            formatters: HashMap::new(),
            handlers: HashMap::new(),
            loggers: HashMap::new(),
            root: None,
        };
        user_config.loggers.insert(
            "myapp.custom".to_string(),
            LoggerConfig {
                handlers: Some(vec!["default".to_string()]),
                level: Some("DEBUG".to_string()),
                propagate: Some(false),
            },
        );

        let merged = merge_with_default(&user_config, "myapp");

        // Default formatters should be present
        assert!(merged.formatters.contains_key("default"));
        assert!(merged.formatters.contains_key("access"));

        // Default handlers should be present
        assert!(merged.handlers.contains_key("default"));
        assert!(merged.handlers.contains_key("access"));

        // Default loggers should be present
        assert!(merged.loggers.contains_key("uvicorn"));
        assert!(merged.loggers.contains_key("uvicorn.error"));
        assert!(merged.loggers.contains_key("uvicorn.access"));
        assert!(merged.loggers.contains_key("myapp"));

        // User's custom logger should also be present
        assert!(merged.loggers.contains_key("myapp.custom"));
    }

    #[test]
    fn test_merge_with_default_overrides_existing() {
        // User config that overrides the uvicorn logger level
        let mut user_config = LoggingConfig {
            version: 1,
            disable_existing_loggers: false,
            formatters: HashMap::new(),
            handlers: HashMap::new(),
            loggers: HashMap::new(),
            root: None,
        };
        user_config.loggers.insert(
            "uvicorn".to_string(),
            LoggerConfig {
                handlers: Some(vec!["default".to_string()]),
                level: Some("DEBUG".to_string()),
                propagate: Some(false),
            },
        );

        let merged = merge_with_default(&user_config, "myapp");

        // uvicorn logger should have user's DEBUG level, not default INFO
        if let Some(uvicorn_logger) = merged.loggers.get("uvicorn") {
            assert_eq!(uvicorn_logger.level, Some("DEBUG".to_string()));
        } else {
            panic!("uvicorn logger should exist");
        }

        // Other default loggers should still be present
        assert!(merged.loggers.contains_key("uvicorn.error"));
        assert!(merged.loggers.contains_key("uvicorn.access"));
        assert!(merged.loggers.contains_key("myapp"));
    }

    #[test]
    fn test_merge_with_default_inline_table_syntax() {
        // Test parsing inline table syntax as shown in documentation
        let toml_str = r#"
[tool.apx.dev.logging]
version = 1
disable_existing_loggers = false

[tool.apx.dev.logging.formatters]
custom = { format = "%(levelname)s %(name)s %(message)s" }

[tool.apx.dev.logging.handlers]
console = { class = "logging.StreamHandler", formatter = "custom", stream = "ext://sys.stdout" }

[tool.apx.dev.logging.loggers]
"uvicorn" = { level = "DEBUG", handlers = ["console"], propagate = false }
"myapp" = { level = "DEBUG", handlers = ["console"], propagate = false }
"#;
        let Ok(value) = toml_str.parse::<toml::Value>() else {
            panic!("failed to parse TOML");
        };
        let Ok(dev_config) = parse_dev_config(&value, Path::new("/test")) else {
            panic!("failed to parse dev config");
        };

        assert!(dev_config.logging.is_some());
        let Some(user_config) = dev_config.logging else {
            panic!("logging config should exist");
        };

        let merged = merge_with_default(&user_config, "testapp");

        // User's custom formatter should be present alongside defaults
        assert!(merged.formatters.contains_key("custom"));
        assert!(merged.formatters.contains_key("default"));
        assert!(merged.formatters.contains_key("access"));

        // User's console handler should override default
        assert!(merged.handlers.contains_key("console"));

        // User's uvicorn logger should override default (DEBUG instead of INFO)
        if let Some(uvicorn_logger) = merged.loggers.get("uvicorn") {
            assert_eq!(uvicorn_logger.level, Some("DEBUG".to_string()));
        } else {
            panic!("uvicorn logger should exist");
        }

        // Default app logger (testapp) should still be present
        assert!(merged.loggers.contains_key("testapp"));

        // User's myapp logger should also be present
        assert!(merged.loggers.contains_key("myapp"));
    }

    /// Test that the default logging config can be loaded by Python's logging.config.dictConfig.
    /// This ensures the generated JSON is valid and doesn't contain unexpected fields like
    /// `filename: null` that would cause StreamHandler to fail.
    #[test]
    fn test_default_config_python_validation() {
        use std::io::Write;

        let config = default_logging_config("testapp");
        let json = serde_json::to_string_pretty(&config).expect("Failed to serialize config");

        // Write config to temp file
        let mut temp_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
        temp_file
            .write_all(json.as_bytes())
            .expect("Failed to write config");
        let config_path = temp_file.path().to_string_lossy().to_string();

        // Validate using Python's logging.config.dictConfig
        let output = std::process::Command::new("uv")
            .args([
                "run",
                "--no-sync",
                "python",
                "-c",
                &format!(
                    "import json, logging.config; logging.config.dictConfig(json.load(open('{}')))",
                    config_path
                ),
            ])
            .output()
            .expect("Failed to run Python validation");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!(
                "Default logging config failed Python validation:\n{}\n\nConfig JSON:\n{}",
                stderr, json
            );
        }
    }
}
