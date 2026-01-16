use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_API_PREFIX: &str = "/api";
const PYPROJECT_FILENAME: &str = "pyproject.toml";

#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    pub app_name: String,
    pub app_slug: String,
    pub app_module: String,
    pub api_prefix: String,
    pub metadata_path: PathBuf,
}

pub fn read_project_metadata(project_root: &Path) -> Result<ProjectMetadata, String> {
    let pyproject_path = project_root.join(PYPROJECT_FILENAME);
    let pyproject_contents = fs::read_to_string(&pyproject_path)
        .map_err(|err| format!("Failed to read pyproject.toml: {err}"))?;
    let pyproject_value: toml::Value = pyproject_contents
        .parse()
        .map_err(|err| format!("Failed to parse pyproject.toml: {err}"))?;

    let metadata = pyproject_value
        .get("tool")
        .and_then(|tool| tool.get("apx"))
        .and_then(|apx| apx.get("metadata"))
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

    Ok(ProjectMetadata {
        app_name,
        app_slug,
        app_module,
        api_prefix,
        metadata_path: PathBuf::from(metadata_path),
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

pub fn ensure_apx_plugin(app_dir: &Path) -> Result<(), String> {
    let apx_dir = app_dir.join(".apx");
    let plugin_path = apx_dir.join("plugin.ts");

    let plugin_contents = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/apx/templates/base/.apx/plugin.ts"
    ));

    if plugin_path.exists() {
        let existing = fs::read_to_string(&plugin_path)
            .map_err(|err| format!("Failed to read .apx/plugin.ts: {err}"))?;
        if existing == plugin_contents {
            return Ok(());
        }
        fs::write(&plugin_path, plugin_contents)
            .map_err(|err| format!("Failed to update .apx/plugin.ts: {err}"))?;
        println!("Updated .apx/plugin.ts from template");
        return Ok(());
    }

    ensure_dir(&apx_dir)?;
    fs::write(&plugin_path, plugin_contents)
        .map_err(|err| format!("Failed to write .apx/plugin.ts: {err}"))?;
    println!("Created .apx/plugin.ts from template");
    Ok(())
}

pub fn bun_install(app_dir: &Path, bun_path: &Path) -> Result<(), String> {
    let mut cmd = Command::new(bun_path);
    cmd.arg("install");
    if let Ok(cache_dir) = std::env::var("BUN_CACHE_DIR") {
        cmd.arg("--cache-dir").arg(cache_dir);
    }
    cmd.current_dir(app_dir);
    let output = cmd
        .output()
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
