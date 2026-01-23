use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;
use indicatif::{ProgressBar, ProgressStyle};

const DEFAULT_API_PREFIX: &str = "/api";
const PYPROJECT_FILENAME: &str = "pyproject.toml";

/// OpenAPI schema directory and filename constants
pub const APX_DIR_NAME: &str = ".apx";
pub const OPENAPI_SCHEMA_FILENAME: &str = "openapi.json";

#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    pub app_name: String,
    pub app_slug: String,
    pub app_module: String,
    pub api_prefix: String,
    pub metadata_path: PathBuf,
    pub ui_root: PathBuf,
    pub ui_registries: HashMap<String, String>,
}

pub fn read_project_metadata(project_root: &Path) -> Result<ProjectMetadata, String> {
    let pyproject_path = project_root.join(PYPROJECT_FILENAME);
    let pyproject_contents = fs::read_to_string(&pyproject_path)
        .map_err(|err| format!("Failed to read pyproject.toml: {err}"))?;
    let pyproject_value: toml::Value = pyproject_contents
        .parse()
        .map_err(|err| format!("Failed to parse pyproject.toml: {err}"))?;

    let apx = pyproject_value
        .get("tool")
        .and_then(|tool| tool.get("apx"))
        .ok_or_else(|| "Missing tool.apx in pyproject.toml".to_string())?;

    let metadata = apx
        .get("metadata")
        .ok_or_else(|| "Missing tool.apx.metadata in pyproject.toml".to_string())?;

    let app_name = get_metadata_string(metadata, "app-name")?;
    let app_slug = get_metadata_string(metadata, "app-slug")?;
    let app_module = get_metadata_string(metadata, "app-module")?;
    let api_prefix = metadata
        .get("api-prefix")
        .and_then(|val| val.as_str())
        .unwrap_or(DEFAULT_API_PREFIX)
        .to_string();
    let metadata_path = get_metadata_string(metadata, "metadata-path")?;

    // Parse UI configuration
    let ui = apx.get("ui");
    
    let ui_root = ui
        .and_then(|u| u.get("root"))
        .and_then(|v| v.as_str())
        .unwrap_or("src/ui")
        .to_string();

    let ui_registries: HashMap<String, String> = ui
        .and_then(|u| u.get("registries"))
        .and_then(|r| r.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(k, v)| {
                    v.as_str().map(|s| (k.clone(), s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ProjectMetadata {
        app_name,
        app_slug,
        app_module,
        api_prefix,
        metadata_path: PathBuf::from(metadata_path),
        ui_root: PathBuf::from(ui_root),
        ui_registries,
    })
}

pub fn write_metadata_file(
    project_root: &Path,
    metadata: &ProjectMetadata,
) -> Result<(), String> {
    let target_path = project_root.join(&metadata.metadata_path);
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create metadata directory: {err}"))?;
    }

    let contents = [
        format!("app_name = \"{}\"", metadata.app_name),
        format!("app_module = \"{}\"", metadata.app_module),
        format!("app_slug = \"{}\"", metadata.app_slug),
        format!("api_prefix = \"{}\"", metadata.api_prefix),
    ]
    .join("\n");

    fs::write(&target_path, contents)
        .map_err(|err| format!("Failed to write metadata file: {err}"))
}

pub fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("Failed to create directory: {err}"))
}

pub async fn bun_install(app_dir: &Path, bun_path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(bun_path);
    cmd.arg("install");
    if let Ok(cache_dir) = std::env::var("BUN_CACHE_DIR") {
        cmd.arg("--cache-dir").arg(cache_dir);
    }
    cmd.current_dir(app_dir);
    let output = cmd
        .output()
        .await
        .map_err(|err| format!("Failed to run bun install: {err}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "bun install failed with status {status}. Stdout: {stdout} Stderr: {stderr}",
            status = output.status
        ));
    }

    Ok(())
}

fn get_metadata_string(metadata: &toml::Value, key: &str) -> Result<String, String> {
    metadata
        .get(key)
        .and_then(|val| val.as_str())
        .map(|val| val.to_string())
        .ok_or_else(|| format!("Missing {key} in pyproject.toml metadata"))
}

// Spinner utilities for CLI operations
pub fn spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(message.to_string());
    spinner
}

pub fn format_elapsed_ms(start: Instant) -> String {
    let elapsed = start.elapsed();
    if elapsed.as_secs() == 0 {
        return format!("{}ms", elapsed.as_millis());
    }
    let seconds = elapsed.as_secs();
    let remaining_ms = elapsed.subsec_millis();
    format!("{seconds}s {remaining_ms}ms")
}

pub fn run_with_spinner<F>(description: &str, success_message: &str, f: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let spinner = spinner(description);
    let start = Instant::now();
    let result = f();
    spinner.finish_and_clear();
    if result.is_ok() {
        println!("{} ({})", success_message, format_elapsed_ms(start));
    }
    result
}

pub async fn run_with_spinner_async<F, Fut>(
    description: &str,
    success_message: &str,
    f: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let spinner = spinner(description);
    let start = Instant::now();
    let result = f().await;
    spinner.finish_and_clear();
    if result.is_ok() {
        println!("{} ({})", success_message, format_elapsed_ms(start));
    }
    result
}
