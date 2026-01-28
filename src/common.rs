use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Command to spawn an apx subprocess without going through `uv run`.
///
/// Since `apx` is a Python entry point (not a standalone binary),
/// `std::env::current_exe()` returns the Python interpreter. We use
/// `python -m apx` to invoke apx as a module, avoiding the extra `uv`
/// wrapper process and uv cache locking.
#[derive(Debug, Clone)]
pub struct ApxCommand {
    /// Path to the Python interpreter
    pub python: PathBuf,
}

impl ApxCommand {
    /// Get the command to spawn apx subprocesses.
    pub fn new() -> Result<Self, String> {
        let python = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;
        Ok(Self { python })
    }

    /// Create a new std::process::Command for spawning apx.
    pub fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.python);
        cmd.args(["-m", "apx"]);
        cmd
    }

    /// Create a new tokio::process::Command for spawning apx.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.python);
        cmd.args(["-m", "apx"]);
        cmd
    }

    /// Format the command for display/logging.
    pub fn display(&self) -> String {
        format!("{} -m apx", self.python.display())
    }
}

const DEFAULT_API_PREFIX: &str = "/api";
const PYPROJECT_FILENAME: &str = "pyproject.toml";

#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    pub app_name: String,
    pub app_slug: String,
    pub app_entrypoint: String,
    pub api_prefix: String,
    pub metadata_path: PathBuf,
    pub ui_root: PathBuf,
    pub ui_registries: HashMap<String, String>,
}

impl ProjectMetadata {
    /// Returns the dist directory path (always __dist__ in the same folder as _metadata.py)
    pub fn dist_dir(&self, project_root: &Path) -> PathBuf {
        let metadata_abs = project_root.join(&self.metadata_path);
        metadata_abs
            .parent()
            .unwrap_or(project_root)
            .join("__dist__")
    }
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
    let app_entrypoint = get_metadata_string(metadata, "app-entrypoint")?;
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
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Ok(ProjectMetadata {
        app_name,
        app_slug,
        app_entrypoint,
        api_prefix,
        metadata_path: PathBuf::from(metadata_path),
        ui_root: PathBuf::from(ui_root),
        ui_registries,
    })
}

pub fn write_metadata_file(project_root: &Path, metadata: &ProjectMetadata) -> Result<(), String> {
    let target_path = project_root.join(&metadata.metadata_path);
    tracing::debug!("Writing metadata file to {}", target_path.display());

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create metadata directory: {err}"))?;
    }

    let contents = [
        "from pathlib import Path".to_string(),
        String::new(),
        format!("app_name = \"{}\"", metadata.app_name),
        format!("app_entrypoint = \"{}\"", metadata.app_entrypoint),
        format!("app_slug = \"{}\"", metadata.app_slug),
        format!("api_prefix = \"{}\"", metadata.api_prefix),
        "dist_dir = Path(__file__).parent / \"__dist__\"".to_string(),
    ]
    .join("\n");

    // Only write if file doesn't exist or contents have changed
    let needs_write = match fs::read_to_string(&target_path) {
        Ok(existing) => existing != contents,
        Err(_) => true, // File doesn't exist or can't be read
    };

    if needs_write {
        fs::write(&target_path, contents)
            .map_err(|err| format!("Failed to write metadata file: {err}"))?;
        tracing::debug!("Metadata file written successfully");
    } else {
        tracing::debug!("Metadata file unchanged, skipping write");
    }

    // Create __dist__ directory and .gitignore
    let dist_dir = metadata.dist_dir(project_root);
    tracing::debug!("Creating dist directory at {}", dist_dir.display());
    ensure_dir(&dist_dir)?;

    let gitignore_path = dist_dir.join(".gitignore");
    if !gitignore_path.exists() {
        fs::write(&gitignore_path, "*\n")
            .map_err(|err| format!("Failed to write __dist__ .gitignore: {err}"))?;
    }

    tracing::debug!("Dist directory and .gitignore created successfully");

    Ok(())
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

/// Run `uv sync` to ensure Python dependencies are installed.
pub async fn uv_sync(app_dir: &Path) -> Result<(), String> {
    tracing::debug!("Running uv sync in {}", app_dir.display());

    let output = Command::new("uv")
        .arg("sync")
        .current_dir(app_dir)
        .output()
        .await
        .map_err(|err| format!("Failed to run uv sync: {err}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "uv sync failed with status {status}. Stdout: {stdout} Stderr: {stderr}",
            status = output.status
        ));
    }

    tracing::debug!("uv sync completed successfully");
    Ok(())
}

/// Generate the `_version.py` file using uv-dynamic-versioning.
/// Generate `_version.py` using uv-dynamic-versioning output.
/// Falls back to writing "0.0.0" if the tool fails.
pub async fn generate_version_file(
    app_dir: &Path,
    metadata: &ProjectMetadata,
) -> Result<(), String> {
    tracing::debug!("Generating version file");

    // Determine the version file path
    let version_path = app_dir
        .join(&metadata.metadata_path)
        .parent()
        .map(|p| p.join("_version.py"))
        .ok_or("Failed to determine version file path")?;

    // Try running uv-dynamic-versioning (outputs version string to stdout)
    let output = Command::new("uv")
        .args(["tool", "run", "uv-dynamic-versioning"])
        .current_dir(app_dir)
        .output()
        .await;

    let version = match output {
        Ok(result) if result.status.success() => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let version = stdout.trim();
            if !version.is_empty() {
                tracing::debug!("uv-dynamic-versioning returned version: {}", version);
                version.to_string()
            } else {
                tracing::warn!("uv-dynamic-versioning returned empty output, using fallback");
                "0.0.0".to_string()
            }
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            tracing::warn!("uv-dynamic-versioning failed: {stderr}, using fallback version");
            "0.0.0".to_string()
        }
        Err(err) => {
            tracing::warn!("Failed to run uv-dynamic-versioning: {err}, using fallback version");
            "0.0.0".to_string()
        }
    };

    // Write the version file
    let content = format!("version = \"{}\"\n", version);
    tracing::debug!("Writing version file to {}", version_path.display());
    std::fs::write(&version_path, content)
        .map_err(|err| format!("Failed to write version file: {err}"))?;

    tracing::debug!("Version file written successfully");
    Ok(())
}

/// Result of preflight check with timing information.
#[derive(Debug)]
#[allow(dead_code)]
pub struct PreflightResult {
    pub metadata: ProjectMetadata,
    pub layout_ms: u128,
    pub uv_sync_ms: u128,
    pub version_ms: u128,
    pub bun_install_ms: Option<u128>,
}

/// Run preflight checks to ensure the project is ready.
///
/// This function should be called before starting the dev server or building the project.
/// It performs the following steps:
/// 1. Verifies project layout (generates `_metadata.py` and creates `__dist__`)
/// 2. Runs `uv sync` to install Python dependencies
/// 3. Generates `_version.py` via uv-dynamic-versioning (with fallback)
/// 4. Runs `bun install` if `node_modules` is missing
///
/// Returns timing information for each step.
pub async fn run_preflight_checks(
    app_dir: &Path,
    bun_path: &Path,
) -> Result<PreflightResult, String> {
    // Step 1: Verify project layout (generates _metadata.py and creates __dist__)
    let layout_start = Instant::now();
    let metadata = read_project_metadata(app_dir)?;
    write_metadata_file(app_dir, &metadata)?;
    let layout_ms = layout_start.elapsed().as_millis();

    // Step 2: Run uv sync to ensure Python deps are installed
    let uv_start = Instant::now();
    uv_sync(app_dir).await?;
    let uv_sync_ms = uv_start.elapsed().as_millis();

    // Step 3: Generate version file
    let version_start = Instant::now();
    generate_version_file(app_dir, &metadata).await?;
    let version_ms = version_start.elapsed().as_millis();

    // Step 4: Run bun install if node_modules is missing
    let node_modules_dir = app_dir.join("node_modules");
    let bun_install_ms = if !node_modules_dir.exists() {
        let bun_start = Instant::now();
        bun_install(app_dir, bun_path).await?;
        Some(bun_start.elapsed().as_millis())
    } else {
        None
    };

    Ok(PreflightResult {
        metadata,
        layout_ms,
        uv_sync_ms,
        version_ms,
        bun_install_ms,
    })
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

/// Timer utility for measuring and logging elapsed time
pub struct Timer {
    start: Instant,
    label: String,
}

impl Timer {
    /// Start a new timer with a label
    pub fn start(label: impl Into<String>) -> Self {
        let label = label.into();
        tracing::debug!("⏱️  [{}] Starting...", label);
        Self {
            start: Instant::now(),
            label,
        }
    }

    /// Log elapsed time and return duration in milliseconds
    pub fn lap(&self, step: &str) -> u128 {
        let elapsed = self.start.elapsed();
        let ms = elapsed.as_millis();
        tracing::info!(
            "⏱️  [{}] {} took {}ms ({:.2}s)",
            self.label,
            step,
            ms,
            elapsed.as_secs_f64()
        );
        ms
    }

    /// Log final elapsed time
    pub fn finish(self) -> u128 {
        let elapsed = self.start.elapsed();
        let ms = elapsed.as_millis();
        tracing::info!(
            "⏱️  [{}] COMPLETED in {}ms ({:.2}s)",
            self.label,
            ms,
            elapsed.as_secs_f64()
        );
        ms
    }
}
