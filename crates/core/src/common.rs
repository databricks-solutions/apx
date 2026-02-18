use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::api_generator::generate_openapi;
use crate::download::{resolve_bun, resolve_uv};
use crate::python_logging::{DevConfig, parse_dev_config};

/// Controls how progress output is displayed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputMode {
    /// CLI: spinners + formatted output to stdout
    Interactive,
    /// MCP / headless: quiet progress to stderr only (nothing on stdout)
    Quiet,
}

/// Dev dependencies required by apx frontend entrypoint.ts
/// These must be installed before running any frontend command
pub const ENTRYPOINT_DEV_DEPS: &[&str] = &[
    "vite",
    "@tailwindcss/vite",
    "@vitejs/plugin-react",
    "@tanstack/router-plugin",
    "@tanstack/router-generator",
    "@opentelemetry/sdk-logs",
    "@opentelemetry/exporter-logs-otlp-http",
    "@opentelemetry/api-logs",
    "@opentelemetry/resources",
];

/// List available Databricks CLI profiles from ~/.databrickscfg
pub fn list_profiles() -> Result<Vec<String>, String> {
    apx_databricks_sdk::list_profile_names().map_err(|e| e.to_string())
}

/// Base command for running tools via `uv run`.
///
/// Provides a consistent way to spawn subprocesses that run within
/// the project's uv-managed Python environment.
#[derive(Debug, Clone)]
pub struct UvCommand {
    tool: &'static str,
    uv_path: PathBuf,
}

impl UvCommand {
    /// Create a new UvCommand for the specified tool.
    /// Resolves uv binary (downloads if needed).
    pub async fn new(tool: &'static str) -> Result<Self, String> {
        let resolved = resolve_uv().await?;
        tracing::debug!(
            "using {} uv: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self {
            tool,
            uv_path: resolved.path,
        })
    }

    /// Create a new tokio::process::Command for spawning the tool via uv.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.uv_path);
        cmd.args(["run", self.tool]);
        cmd
    }

    /// Format the command for display/logging.
    pub fn display(&self) -> String {
        format!("uv run {}", self.tool)
    }
}

/// Command to spawn apx subprocesses via `uv run apx`.
///
/// Uses uv to ensure the correct Python environment is used,
/// regardless of which Python installations are available on the system.
#[derive(Debug, Clone)]
pub struct ApxCommand {
    inner: UvCommand,
}

impl ApxCommand {
    /// Create a new ApxCommand instance.
    pub async fn new() -> Result<Self, String> {
        Ok(Self {
            inner: UvCommand::new("apx").await?,
        })
    }

    /// Create a new tokio::process::Command for spawning apx.
    pub fn tokio_command(&self) -> tokio::process::Command {
        self.inner.tokio_command()
    }

    /// Format the command for display/logging.
    pub fn display(&self) -> String {
        self.inner.display()
    }
}

/// Command to spawn bun using a resolved bun binary.
///
/// Resolution order: env override → system PATH → auto-download.
#[derive(Debug, Clone)]
pub struct BunCommand {
    bun_path: PathBuf,
}

impl BunCommand {
    /// Create a new BunCommand instance.
    /// Resolves bun binary (downloads if needed).
    pub async fn new() -> Result<Self, String> {
        let resolved = resolve_bun().await?;
        tracing::debug!(
            "using {} bun: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self {
            bun_path: resolved.path,
        })
    }

    /// Get the path to the resolved bun binary.
    pub fn path(&self) -> &Path {
        &self.bun_path
    }

    /// Build a PATH with the apx bin directory prepended.
    /// This ensures child processes spawned by bun also use the apx-bundled bun.
    fn patched_path(&self) -> std::ffi::OsString {
        let apx_bin_dir = self.bun_path.parent().unwrap_or(std::path::Path::new(""));
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![apx_bin_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current_path));
        std::env::join_paths(paths).unwrap_or(current_path)
    }

    /// Create a new std::process::Command for spawning bun.
    #[allow(dead_code)]
    pub fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.bun_path);
        cmd.env("PATH", self.patched_path());
        cmd
    }

    /// Create a new tokio::process::Command for spawning bun.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.bun_path);
        cmd.env("PATH", self.patched_path());
        cmd
    }

    /// Create a tokio command with NODE_PATH set to `<app_dir>/node_modules`.
    ///
    /// Use this when running scripts that live outside the project directory
    /// (e.g. the bundled entrypoint.ts at ~/.apx/files/). Without NODE_PATH,
    /// bun resolves transitive dependencies relative to the script's location
    /// or its global cache, which fails to find packages installed in the
    /// project's node_modules.
    pub fn tokio_command_with_node_path(&self, app_dir: &Path) -> tokio::process::Command {
        let mut cmd = self.tokio_command();
        cmd.env("NODE_PATH", app_dir.join("node_modules"));
        cmd
    }

    /// Format the command for display/logging.
    #[allow(dead_code)]
    pub fn display(&self) -> String {
        format!("bun ({})", self.bun_path.display())
    }
}

/// Handle spawn errors with user-friendly messages.
/// Call this when a Command::spawn() fails to provide actionable feedback.
pub fn handle_spawn_error(tool: &str, error: std::io::Error) -> String {
    let msg = if error.kind() == std::io::ErrorKind::NotFound {
        format!(
            "Failed to spawn '{}': executable not found. \
             Make sure '{}' is installed and available in PATH.",
            tool,
            if tool == "apx" || tool == "uvicorn" {
                "uv"
            } else {
                tool
            }
        )
    } else {
        format!("Failed to spawn '{tool}': {error}")
    };
    eprintln!("{msg}");
    msg
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
    pub ui_root: Option<PathBuf>,
    pub ui_registries: Option<HashMap<String, String>>,
    pub dev_config: DevConfig,
}

impl ProjectMetadata {
    /// Returns true if this project has a frontend (UI).
    pub fn has_ui(&self) -> bool {
        self.ui_root.is_some()
    }

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

    // Parse UI configuration — None when [tool.apx.ui] section is absent
    let ui = apx.get("ui");

    let ui_root = ui.map(|u| {
        let root = u.get("root").and_then(|v| v.as_str()).unwrap_or("src/ui");
        PathBuf::from(root)
    });

    let ui_registries = ui.map(|u| {
        u.get("registries")
            .and_then(|r| r.as_table())
            .map(|table| {
                table
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    });

    // Parse dev configuration
    let dev_config = parse_dev_config(&pyproject_value, project_root)?;

    Ok(ProjectMetadata {
        app_name,
        app_slug,
        app_entrypoint,
        api_prefix,
        metadata_path: PathBuf::from(metadata_path),
        ui_root,
        ui_registries,
        dev_config,
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

pub async fn bun_install(app_dir: &Path) -> Result<(), String> {
    let bun = BunCommand::new().await?;
    tracing::debug!(
        bun_path = %bun.path().display(),
        app_dir = %app_dir.display(),
        "Running bun install"
    );
    let mut cmd = bun.tokio_command();
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

/// Ensure all entrypoint.ts dependencies are installed.
/// Runs `bun add --dev` for required dependencies (idempotent - safe if already installed).
pub async fn ensure_entrypoint_deps(app_dir: &Path) -> Result<(), String> {
    tracing::debug!(
        bun_deps = ENTRYPOINT_DEV_DEPS.join(", "),
        app_dir = %app_dir.display(),
        "Ensuring frontend dependencies"
    );

    // Run bun add --dev for all dependencies (idempotent operation)
    let bun = BunCommand::new().await?;
    tracing::debug!(bun_path = %bun.path().display(), "Running bun add --dev");
    let mut cmd = bun.tokio_command();
    cmd.arg("add").arg("--dev");
    for dep in ENTRYPOINT_DEV_DEPS {
        cmd.arg(*dep);
    }
    cmd.current_dir(app_dir);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run bun add: {e}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Failed to install frontend dependencies (exit {}):\n\
             bun: {}\n\
             stdout:\n{stdout}\n\
             stderr:\n{stderr}",
            output
                .status
                .code()
                .map_or("signal".into(), |c: i32| c.to_string()),
            bun.path().display(),
        ));
    }

    tracing::debug!("Frontend dependencies installed successfully");
    Ok(())
}

/// Run `uv sync` to ensure Python dependencies are installed.
pub async fn uv_sync(app_dir: &Path) -> Result<(), String> {
    tracing::debug!("Running uv sync in {}", app_dir.display());

    let uv_path = resolve_uv().await?.path;
    let output = Command::new(&uv_path)
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
    let uv_path = resolve_uv().await?.path;
    let output = Command::new(&uv_path)
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
    let content = format!("version = \"{version}\"\n");
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
    pub openapi_ms: u128,
    pub version_ms: u128,
    pub bun_install_ms: Option<u128>,
    pub has_ui: bool,
}

/// Run preflight checks to ensure the project is ready.
///
/// This function should be called before starting the dev server or building the project.
/// It performs the following steps:
/// 1. Verifies project layout (generates `_metadata.py` and creates `__dist__`)
/// 2. Runs `uv sync` to install Python dependencies
/// 3. Generates OpenAPI client (`lib/api.ts`) from the backend
/// 4. Generates `_version.py` via uv-dynamic-versioning (with fallback)
/// 5. Runs `bun install` if `node_modules` is missing
///
/// Returns timing information for each step.
pub async fn run_preflight_checks(app_dir: &Path) -> Result<PreflightResult, String> {
    // Step 1: Verify project layout (generates _metadata.py and creates __dist__)
    let layout_start = Instant::now();
    let metadata = read_project_metadata(app_dir)?;
    write_metadata_file(app_dir, &metadata)?;
    let layout_ms = layout_start.elapsed().as_millis();

    // Step 2: Run uv sync to ensure Python deps are installed
    let uv_start = Instant::now();
    uv_sync(app_dir).await?;
    let uv_sync_ms = uv_start.elapsed().as_millis();

    // Step 3: Generate OpenAPI client (requires Python deps from step 2, only for projects with UI)
    let openapi_ms = if metadata.has_ui() {
        let openapi_start = Instant::now();
        generate_openapi(app_dir).await?;
        openapi_start.elapsed().as_millis()
    } else {
        0
    };

    // Step 4: Generate version file
    let version_start = Instant::now();
    generate_version_file(app_dir, &metadata).await?;
    let version_ms = version_start.elapsed().as_millis();

    // Step 5: Run bun install if node_modules is missing (only for projects with UI)
    let bun_install_ms = if metadata.has_ui() {
        let node_modules_dir = app_dir.join("node_modules");
        if !node_modules_dir.exists() {
            let bun_start = Instant::now();
            bun_install(app_dir).await?;
            Some(bun_start.elapsed().as_millis())
        } else {
            None
        }
    } else {
        None
    };

    let has_ui = metadata.has_ui();
    Ok(PreflightResult {
        metadata,
        layout_ms,
        uv_sync_ms,
        openapi_ms,
        version_ms,
        bun_install_ms,
        has_ui,
    })
}

fn get_metadata_string(metadata: &toml::Value, key: &str) -> Result<String, String> {
    metadata
        .get(key)
        .and_then(|val| val.as_str())
        .map(|val| val.to_string())
        .ok_or_else(|| format!("Missing {key} in pyproject.toml metadata"))
}

/// Print a message to stdout (Interactive) or stderr (Quiet).
#[allow(clippy::print_stdout)]
pub fn emit(mode: OutputMode, msg: &str) {
    match mode {
        OutputMode::Interactive => println!("{msg}"),
        OutputMode::Quiet => eprintln!("{msg}"),
    }
}

/// Create a spinner appropriate for the given output mode.
/// Interactive: visible spinner on stdout. Quiet: hidden (no output).
pub fn spinner_for_mode(message: &str, mode: OutputMode) -> ProgressBar {
    match mode {
        OutputMode::Interactive => spinner(message),
        OutputMode::Quiet => {
            let pb = ProgressBar::hidden();
            pb.set_message(message.to_string());
            pb
        }
    }
}

// Spinner utilities for CLI operations
#[allow(clippy::print_stdout)]
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

/// Output captured from a streaming command.
#[derive(Debug, Default)]
pub struct StreamingOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Run a command, stream its output to a progress bar, AND capture output for error reporting.
/// Returns the captured output on success, or an error with full output on failure.
pub async fn run_command_streaming_with_output(
    mut cmd: Command,
    spinner: &ProgressBar,
    prefix: &str,
    error_msg: &str,
) -> Result<StreamingOutput, String> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| format!("{error_msg}: {err}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let prefix_stdout = prefix.to_string();
    let prefix_stderr = prefix.to_string();
    let spinner_stdout = spinner.clone();
    let spinner_stderr = spinner.clone();

    // Spawn tasks to read stdout and stderr concurrently, capturing all output
    let stdout_task = tokio::spawn(async move {
        let mut captured = Vec::new();
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                captured.push(line.clone());
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    spinner_stdout.set_message(format!("{prefix_stdout} {trimmed}"));
                }
            }
        }
        captured.join("\n")
    });

    let stderr_task = tokio::spawn(async move {
        let mut captured = Vec::new();
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                captured.push(line.clone());
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    spinner_stderr.set_message(format!("{prefix_stderr} {trimmed}"));
                }
            }
        }
        captured.join("\n")
    });

    // Wait for both readers and the process to complete
    let (stdout_result, stderr_result) = tokio::join!(stdout_task, stderr_task);

    let output = StreamingOutput {
        stdout: stdout_result.unwrap_or_default(),
        stderr: stderr_result.unwrap_or_default(),
    };

    let status = child
        .wait()
        .await
        .map_err(|err| format!("{error_msg}: {err}"))?;

    if !status.success() {
        let mut full_error = format!("{error_msg}: exit code {}", status.code().unwrap_or(-1));

        if !output.stderr.is_empty() {
            full_error.push_str(&format!("\n\nStderr:\n{}", output.stderr));
        }

        if !output.stdout.is_empty() {
            full_error.push_str(&format!("\n\nStdout:\n{}", output.stdout));
        }

        return Err(full_error);
    }

    Ok(output)
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

#[allow(clippy::print_stdout)]
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

#[allow(clippy::print_stdout)]
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
#[derive(Debug)]
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
