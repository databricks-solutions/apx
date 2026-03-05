use clap::Args;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::debug;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::api_generator::generate_openapi;
use apx_core::common::{
    ensure_dir, format_elapsed_ms, run_command_streaming_with_output, run_preflight_checks, spinner,
};
use apx_core::external::uv::Uv;

const DEFAULT_BUILD_DIR: &str = ".build";
const DEFAULT_FALLBACK_VERSION: &str = "0.0.0";
const APP_CONFIG_FILES: [&str; 2] = ["app.yml", "app.yaml"];

#[derive(Args, Debug, Clone)]
pub struct BuildArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        long = "build-path",
        default_value = DEFAULT_BUILD_DIR,
        help = "Path to the build directory where artifacts will be placed, relative to the app path"
    )]
    pub build_path: PathBuf,
    #[arg(long = "skip-ui-build", help = "Skip the UI build step")]
    pub skip_ui_build: bool,
    /// Python app module for framework manifest (e.g. "backend.app").
    /// If provided, compiles a manifest.json into the build directory.
    #[arg(long = "app")]
    pub app_module: Option<String>,
}

pub async fn run(args: BuildArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: BuildArgs) -> Result<(), String> {
    let app_path = find_app_dir(args.app_path)?;
    let build_dir = app_path.join(&args.build_path);

    println!("Building project in {}", app_path.display());

    // Run preflight checks: generate _metadata.py, __dist__, uv sync, version file, bun install if needed
    debug!("Running preflight checks before build");
    let _preflight = run_preflight_checks(&app_path).await?;

    // Set up build directory
    if build_dir.exists() {
        fs::remove_dir_all(&build_dir)
            .map_err(|err| format!("Failed to remove build directory: {err}"))?;
    }
    ensure_dir(&build_dir)?;
    fs::write(build_dir.join(".gitignore"), "*\n")
        .map_err(|err| format!("Failed to write build .gitignore: {err}"))?;

    generate_openapi(&app_path).await?;

    // Compile framework manifest (explicit --app flag or auto-detected from config)
    let app_module = args.app_module.or_else(|| detect_app_module(&app_path));
    if let Some(ref module) = app_module {
        compile_manifest_step(&app_path, &build_dir, module).await?;
    }

    if args.skip_ui_build {
        println!("Skipping UI build");
    } else {
        build_ui(&app_path).await?;
    }

    build_wheel(&app_path, &args.build_path).await?;
    copy_app_config_files(&app_path, &build_dir)?;

    let wheel_file = find_wheel_file(&build_dir)?;
    let requirements_path = build_dir.join("requirements.txt");
    fs::write(&requirements_path, format!("{wheel_file}\n"))
        .map_err(|err| format!("Failed to write requirements.txt: {err}"))?;

    println!("Build completed");
    Ok(())
}

async fn build_ui(app_path: &Path) -> Result<(), String> {
    crate::frontend::build::run_build(app_path, true).await
}

async fn build_wheel(app_path: &Path, build_path: &Path) -> Result<(), String> {
    let start_time = Instant::now();
    let sp = spinner("🐍 Building Python wheel...");

    let base_version = get_base_version(app_path).await;
    let build_version = generate_build_version(&base_version);

    let uv = Uv::new().await?;
    let mut cmd = uv.build_wheel_command(app_path, build_path).into_command();
    cmd.env("UV_DYNAMIC_VERSIONING_BYPASS", build_version);

    let result =
        run_command_streaming_with_output(cmd, &sp, "🐍 Wheel:", "Failed to build Python wheel")
            .await;

    sp.finish_and_clear();

    match result {
        Ok(_) => {
            println!("✅ Python wheel built in {}", format_elapsed_ms(start_time));
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn copy_app_config_files(app_path: &Path, build_dir: &Path) -> Result<(), String> {
    for app_file_name in APP_CONFIG_FILES {
        let app_file = app_path.join(app_file_name);
        if app_file.exists() {
            ensure_dir(build_dir)?;
            fs::copy(&app_file, build_dir.join(app_file_name))
                .map_err(|err| format!("Failed to copy {app_file_name}: {err}"))?;
            break;
        }
    }
    Ok(())
}

fn find_wheel_file(build_dir: &Path) -> Result<String, String> {
    let mut wheel_files = Vec::new();
    for entry in
        fs::read_dir(build_dir).map_err(|err| format!("Failed to read build directory: {err}"))?
    {
        let entry = entry.map_err(|err| format!("Failed to read build entry: {err}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("whl") {
            wheel_files.push(path);
        }
    }

    if wheel_files.is_empty() {
        return Err("No wheel file found in build directory".to_string());
    }

    let wheel_file = wheel_files.remove(0);
    wheel_file
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .ok_or_else(|| "Invalid wheel file name".to_string())
}

async fn get_base_version(app_path: &Path) -> String {
    let Ok(uv) = Uv::new().await else {
        return DEFAULT_FALLBACK_VERSION.to_string();
    };
    match uv.run_hatch_version(app_path).await {
        Ok(version) if !version.is_empty() => version,
        _ => DEFAULT_FALLBACK_VERSION.to_string(),
    }
}

fn generate_build_version(base_version: &str) -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    if base_version.contains('+') {
        format!("{base_version}.{timestamp}")
    } else {
        format!("{base_version}+{timestamp}")
    }
}

async fn compile_manifest_step(
    app_path: &Path,
    build_dir: &Path,
    app_module: &str,
) -> Result<(), String> {
    let start_time = Instant::now();
    let sp = spinner("Compiling framework manifest...");

    let manifest =
        crate::compile_manifest::compile_manifest(app_path, build_dir, app_module).await?;

    sp.finish_and_clear();
    println!(
        "Manifest compiled in {} ({} routes)",
        format_elapsed_ms(start_time),
        manifest.routes.len(),
    );
    Ok(())
}

/// Try to detect the app module from project config files.
///
/// Looks for `command:` lines containing `apx serve --app <module>` in
/// `app.yml` / `app.yaml`.
fn detect_app_module(app_path: &Path) -> Option<String> {
    for name in &APP_CONFIG_FILES {
        let config_path = app_path.join(name);
        let Ok(content) = fs::read_to_string(&config_path) else {
            continue;
        };
        if let Some(module) = parse_app_module_from_config(&content) {
            return Some(module);
        }
    }
    None
}

/// Extract app module from a config file's `command:` line.
///
/// Matches patterns like `command: apx serve --app backend.app`.
fn parse_app_module_from_config(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("command:") {
            continue;
        }
        let value = trimmed.strip_prefix("command:")?.trim().trim_matches('"');
        return extract_app_flag(value);
    }
    None
}

/// Extract the `--app <module>` value from a command string.
fn extract_app_flag(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let idx = parts.iter().position(|&p| p == "--app")?;
    parts.get(idx + 1).map(|s| (*s).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_app_module_from_config_found() {
        let content = "command: apx serve --app backend.app\n";
        assert_eq!(
            parse_app_module_from_config(content),
            Some("backend.app".to_string())
        );
    }

    #[test]
    fn parse_app_module_from_config_quoted() {
        let content = "command: \"apx serve --app my_app.main\"\n";
        assert_eq!(
            parse_app_module_from_config(content),
            Some("my_app.main".to_string())
        );
    }

    #[test]
    fn parse_app_module_from_config_no_app_flag() {
        let content = "command: uvicorn backend.app:app\n";
        assert_eq!(parse_app_module_from_config(content), None);
    }

    #[test]
    fn parse_app_module_from_config_no_command() {
        let content = "name: my-app\nport: 8080\n";
        assert_eq!(parse_app_module_from_config(content), None);
    }

    #[test]
    fn extract_app_flag_present() {
        assert_eq!(
            extract_app_flag("apx serve --app backend.app --port 8000"),
            Some("backend.app".to_string())
        );
    }

    #[test]
    fn extract_app_flag_missing() {
        assert_eq!(extract_app_flag("apx serve --port 8000"), None);
    }

    #[test]
    fn extract_app_flag_at_end() {
        assert_eq!(
            extract_app_flag("apx serve --app backend.app"),
            Some("backend.app".to_string())
        );
    }

    #[test]
    fn extract_app_flag_no_value() {
        assert_eq!(extract_app_flag("apx serve --app"), None);
    }
}
