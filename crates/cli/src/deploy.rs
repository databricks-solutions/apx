//! `apx deploy` — build and deploy the app to Databricks Apps.
//!
//! Flow:
//!   1. Build wheel (skips UI for agent-only apps, respects UV_OFFLINE)
//!   2. Copy pyproject.toml into .build/ (needed for agent config at runtime)
//!   3. Remove .build/.gitignore so DABs syncs all files
//!   4. `databricks bundle deploy --auto-approve` (uploads .build/ to workspace)
//!   5. `databricks apps deploy <slug> --source-code-path <workspace_path>`
//!   6. Poll `databricks apps get` until RUNNING

use clap::Args;
use indicatif::ProgressBar;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tracing::debug;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::{format_elapsed_ms, read_project_metadata, spinner};
use apx_core::dotenv::DotenvFile;

/// Default build output directory, relative to app path.
const DEFAULT_BUILD_DIR: &str = ".build";
/// How long to wait between deployment status polls (ms).
const POLL_INTERVAL_MS: u64 = 3_000;
/// How many times to poll before giving up (~3 min total).
const POLL_MAX_ATTEMPTS: u32 = 60;

#[derive(Args, Debug, Clone)]
pub struct DeployArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "Path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,

    #[arg(
        long = "build-path",
        default_value = DEFAULT_BUILD_DIR,
        help = "Path to the build directory, relative to app path"
    )]
    pub build_path: PathBuf,

    #[arg(
        long = "skip-build",
        help = "Skip the build step and deploy whatever is in the build directory"
    )]
    pub skip_build: bool,

    #[arg(
        long = "profile",
        help = "Databricks CLI profile to use. Defaults to DATABRICKS_CONFIG_PROFILE from .env"
    )]
    pub profile: Option<String>,
}

pub async fn run(args: DeployArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: DeployArgs) -> Result<(), String> {
    let app_path = find_app_dir(args.app_path)?;
    let build_dir = app_path.join(&args.build_path);

    // --- 1. Build ---
    if !args.skip_build {
        crate::build::run_build(&app_path, &build_dir).await?;
    } else if !build_dir.exists() {
        return Err(format!(
            "--skip-build specified but build directory does not exist: {}",
            build_dir.display()
        ));
    }

    // --- 2. Post-build fixups ---
    // Copy pyproject.toml so the agent config is available at runtime.
    let pyproject_src = app_path.join("pyproject.toml");
    if pyproject_src.exists() {
        fs::copy(&pyproject_src, build_dir.join("pyproject.toml"))
            .map_err(|e| format!("Failed to copy pyproject.toml to build dir: {e}"))?;
    }
    // Remove .gitignore — it contains "*" which blocks DABs sync.
    let gitignore = build_dir.join(".gitignore");
    if gitignore.exists() {
        fs::remove_file(&gitignore)
            .map_err(|e| format!("Failed to remove build .gitignore: {e}"))?;
    }

    // --- 3. Read metadata + resolve profile ---
    let meta = read_project_metadata(&app_path)?;
    let app_slug = &meta.app_name;

    let profile = match args.profile {
        Some(p) => p,
        None => {
            let dotenv = DotenvFile::read(&app_path.join(".env"))?;
            let vars = dotenv.get_vars();
            vars.get("DATABRICKS_CONFIG_PROFILE")
                .cloned()
                .ok_or_else(|| {
                    "No Databricks profile found. Set DATABRICKS_CONFIG_PROFILE in .env \
                     or pass --profile"
                        .to_string()
                })?
        }
    };

    debug!("deploying app={app_slug} profile={profile}");

    let sp = spinner(&format!("🚀 Deploying {app_slug}..."));
    let deploy_start = Instant::now();

    // --- 4. Bundle deploy (uploads .build/ to workspace) ---
    sp.set_message(format!("📦 Uploading {app_slug} to workspace..."));
    run_bundle_deploy(&app_path, &profile, &sp)?;

    // --- 5. App deploy (create code deployment from workspace path) ---
    sp.set_message(format!("🔗 Creating app deployment for {app_slug}..."));
    let workspace_path = resolve_workspace_path(&app_path, &profile, &args.build_path)?;
    debug!("workspace_path={workspace_path}");
    run_app_deploy(app_slug, &workspace_path, &profile, &sp)?;

    sp.set_message(format!("⏳ Waiting for {app_slug} to reach RUNNING..."));

    // --- 6. Poll until RUNNING ---
    poll_until_running(app_slug, &profile, &sp).await?;

    sp.finish_and_clear();
    println!(
        "✅ Deployed {} in {}",
        app_slug,
        format_elapsed_ms(deploy_start)
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Bundle deploy
// ---------------------------------------------------------------------------

fn run_bundle_deploy(app_path: &Path, profile: &str, sp: &ProgressBar) -> Result<(), String> {
    sp.set_message("📦 Running bundle deploy...".to_string());

    let output = Command::new("databricks")
        .args([
            "bundle",
            "deploy",
            "--auto-approve",
            "--profile",
            profile,
        ])
        .current_dir(app_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run `databricks` CLI: {e}\n\
                 Make sure the Databricks CLI is installed: https://docs.databricks.com/dev-tools/cli/install.html"
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "databricks bundle deploy failed:\n{stdout}{stderr}"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Workspace path resolution
// ---------------------------------------------------------------------------

/// Minimal subset of databricks.yml we need.
#[derive(Deserialize, Debug)]
struct BundleConfig {
    bundle: BundleSection,
    #[serde(default)]
    targets: std::collections::HashMap<String, TargetSection>,
}

#[derive(Deserialize, Debug)]
struct BundleSection {
    name: String,
}

#[derive(Deserialize, Debug, Default)]
struct TargetSection {
    #[serde(default)]
    default: bool,
}

/// Derive the workspace source-code path from `databricks.yml` + auth info.
///
/// Format: `/Workspace/Users/<user>/.bundle/<bundle_name>/<target>/files/<build_path>`
fn resolve_workspace_path(
    app_path: &Path,
    profile: &str,
    build_path: &Path,
) -> Result<String, String> {
    // Read databricks.yml
    let bundle_config_path = app_path.join("databricks.yml");
    if !bundle_config_path.exists() {
        return Err(
            "databricks.yml not found. Run `databricks bundle init` or use --skip-bundle."
                .to_string(),
        );
    }
    let bundle_yaml = fs::read_to_string(&bundle_config_path)
        .map_err(|e| format!("Failed to read databricks.yml: {e}"))?;
    let bundle_config: BundleConfig = serde_yaml::from_str(&bundle_yaml)
        .map_err(|e| format!("Failed to parse databricks.yml: {e}"))?;

    let bundle_name = &bundle_config.bundle.name;

    // Find the default target (or fall back to "dev")
    let target = bundle_config
        .targets
        .iter()
        .find(|(_, v)| v.default)
        .map(|(k, _)| k.as_str())
        .unwrap_or("dev");

    // Get current user from auth
    let user = get_auth_user(profile)?;

    let build_path_str = build_path.to_string_lossy();
    // Strip leading ./ if present
    let build_path_clean = build_path_str.trim_start_matches("./");

    Ok(format!(
        "/Workspace/Users/{user}/.bundle/{bundle_name}/{target}/files/{build_path_clean}"
    ))
}

/// Call `databricks auth describe --profile <profile>` and extract the username.
fn get_auth_user(profile: &str) -> Result<String, String> {
    let output = Command::new("databricks")
        .args(["auth", "describe", "--profile", profile, "--output", "json"])
        .output()
        .map_err(|e| format!("Failed to call `databricks auth describe`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("databricks auth describe failed: {stderr}"));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse auth describe output: {e}"))?;

    json.get("user")
        .and_then(|u| u.get("userName"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .or_else(|| {
            // Fall back to top-level "username" field
            json.get("username")
                .and_then(|n| n.as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            format!(
                "Could not find username in `databricks auth describe` output: {}",
                serde_json::to_string_pretty(&json).unwrap_or_default()
            )
        })
}

// ---------------------------------------------------------------------------
// App deploy
// ---------------------------------------------------------------------------

fn run_app_deploy(
    app_slug: &str,
    workspace_path: &str,
    profile: &str,
    sp: &ProgressBar,
) -> Result<(), String> {
    sp.set_message(format!("🔗 Deploying code for {app_slug}..."));

    let output = Command::new("databricks")
        .args([
            "apps",
            "deploy",
            app_slug,
            "--source-code-path",
            workspace_path,
            "--profile",
            profile,
        ])
        .output()
        .map_err(|e| format!("Failed to run `databricks apps deploy`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "databricks apps deploy failed:\n{stdout}{stderr}"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Poll until RUNNING
// ---------------------------------------------------------------------------

async fn poll_until_running(app_slug: &str, profile: &str, sp: &ProgressBar) -> Result<(), String> {
    for attempt in 1..=POLL_MAX_ATTEMPTS {
        tokio::time::sleep(tokio::time::Duration::from_millis(POLL_INTERVAL_MS)).await;

        let output = Command::new("databricks")
            .args([
                "apps",
                "get",
                app_slug,
                "--profile",
                profile,
                "--output",
                "json",
            ])
            .output()
            .map_err(|e| format!("Failed to poll deployment status: {e}"))?;

        if !output.status.success() {
            debug!("poll attempt {attempt}: status command failed, retrying");
            continue;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let state = parse_app_state(&stdout);

        debug!("poll attempt {attempt}: state={state:?}");

        match state.as_deref() {
            Some("RUNNING") => return Ok(()),
            Some("ERROR" | "CRASHED") => {
                return Err(format!(
                    "App {app_slug} entered state {}: check `databricks apps logs {app_slug} --profile {profile}`",
                    state.unwrap_or_default()
                ));
            }
            _ => {
                sp.set_message(format!(
                    "⏳ [{attempt}/{POLL_MAX_ATTEMPTS}] Waiting for {app_slug} ({})...",
                    state.as_deref().unwrap_or("unknown")
                ));
            }
        }
    }

    Err(format!(
        "Timed out waiting for {app_slug} to reach RUNNING. \
         Check status with: databricks apps get {app_slug} --profile {profile}"
    ))
}

fn parse_app_state(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("app_status")?
        .get("state")?
        .as_str()
        .map(str::to_uppercase)
}
