//! `apx deploy` — build and deploy the app to Databricks Apps.

use clap::Args;
use indicatif::ProgressBar;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use tracing::debug;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::{format_elapsed_ms, read_project_metadata, run_preflight_checks, spinner};
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

    // --- 2. Read metadata ---
    let _preflight = run_preflight_checks(&app_path).await?;
    let meta = read_project_metadata(&app_path)?;
    let app_slug = &meta.app_slug;

    // --- 3. Resolve Databricks profile ---
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

    debug!("deploying app={app_slug} profile={profile} source={}", build_dir.display());

    // --- 4. Deploy ---
    let sp = spinner(&format!("🚀 Deploying {app_slug}..."));
    let deploy_start = Instant::now();

    run_databricks_deploy(app_slug, &build_dir, &profile, &sp)?;

    sp.set_message(format!("⏳ Waiting for {app_slug} to reach RUNNING..."));

    // --- 5. Poll until RUNNING ---
    poll_until_running(app_slug, &profile, &sp).await?;

    sp.finish_and_clear();
    println!(
        "✅ Deployed {} in {}",
        app_slug,
        format_elapsed_ms(deploy_start)
    );

    Ok(())
}

/// Run `databricks apps deploy <app> --source-code-path <dir> --profile <profile>`.
fn run_databricks_deploy(
    app_slug: &str,
    build_dir: &PathBuf,
    profile: &str,
    sp: &ProgressBar,
) -> Result<(), String> {
    sp.set_message(format!("📦 Uploading {app_slug} to Databricks..."));

    let output = Command::new("databricks")
        .args([
            "apps",
            "deploy",
            app_slug,
            "--source-code-path",
            &build_dir.to_string_lossy(),
            "--profile",
            profile,
        ])
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
            "databricks apps deploy failed:\n{stdout}{stderr}"
        ));
    }

    Ok(())
}

/// Poll `databricks apps get` until the app reaches RUNNING or we time out.
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

/// Extract `app_status.state` from the JSON output of `databricks apps get`.
fn parse_app_state(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("app_status")?
        .get("state")?
        .as_str()
        .map(str::to_uppercase)
}
