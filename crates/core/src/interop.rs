use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing::debug;

use crate::dev::common::{CLIENT_HOST, lock_path, read_lock};
use crate::download::try_resolve_uv;
use crate::resources;

#[cfg(target_os = "windows")]
const AGENT_FILENAME: &str = "apx-agent.exe";
#[cfg(not(target_os = "windows"))]
const AGENT_FILENAME: &str = "apx-agent";

/// Resolve the path to the bundled apx-agent binary.
///
/// Resolution order:
/// 1. `APX_AGENT_PATH` env var (explicit override)
/// 2. `~/.apx/apx-agent` (standard install target)
/// 3. Exe-relative: `<exe_dir>/apx_binaries/apx-agent`
pub fn resolve_apx_agent_binary_path() -> Result<PathBuf, String> {
    // 1. Explicit env var override
    if let Ok(path) = std::env::var("APX_AGENT_PATH") {
        let p = PathBuf::from(&path);
        if p.is_file() {
            debug!(
                "resolve_apx_agent_binary_path: using APX_AGENT_PATH={}",
                p.display()
            );
            return Ok(p);
        }
        return Err(format!("APX_AGENT_PATH={path} does not exist"));
    }

    // 2. Extract embedded agent to ~/.apx/apx-agent
    match resources::ensure_agent_extracted() {
        Ok(p) => {
            debug!(
                "resolve_apx_agent_binary_path: using embedded agent at {}",
                p.display()
            );
            return Ok(p);
        }
        Err(e) => {
            debug!("resolve_apx_agent_binary_path: failed to extract embedded agent: {e}");
        }
    }

    // 3. Exe-relative: <exe_dir>/apx_binaries/apx-agent (fallback for compatibility)
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let candidate = exe_dir.join("apx_binaries").join(AGENT_FILENAME);
        if candidate.is_file() {
            debug!(
                "resolve_apx_agent_binary_path: found exe-relative at {}",
                candidate.display()
            );
            return Ok(candidate);
        }
    }

    Err(format!(
        "Could not find apx-agent binary. Set APX_AGENT_PATH or install it to ~/.apx/{AGENT_FILENAME}."
    ))
}

/// Get the path to the frontend entrypoint.ts asset (materialized from embedded resources)
pub fn frontend_entrypoint_path() -> Result<PathBuf, String> {
    resources::entrypoint_ts_path()
}

/// Extract embedded templates to the versioned cache directory and return the path.
pub fn extract_templates() -> Result<PathBuf, String> {
    resources::templates_dir()
}

pub async fn generate_openapi_spec(
    project_root: &Path,
    app_entrypoint: &str,
    app_slug: &str,
) -> Result<(String, String), String> {
    // Try to fetch from running server first (200ms timeout)
    if let Some(spec_json) = try_fetch_openapi_from_server(project_root).await {
        debug!("Got OpenAPI spec from running server");
        return Ok((spec_json, app_slug.to_string()));
    }

    // Fall back to subprocess method
    generate_openapi_spec_from_module(project_root, app_entrypoint, app_slug).await
}

/// Try to fetch OpenAPI spec from a running dev server.
/// Returns None if server is not running or doesn't respond within 200ms.
async fn try_fetch_openapi_from_server(project_root: &Path) -> Option<String> {
    let lock_file = lock_path(project_root);
    let lock = read_lock(&lock_file).ok()?;

    let url = format!("http://{}:{}/openapi.json", CLIENT_HOST, lock.port);
    debug!("Trying to fetch OpenAPI from server at {}", url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .ok()?;

    let response = client.get(&url).send().await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    response.text().await.ok()
}

/// Generate OpenAPI spec by running a Python subprocess via `uv run`.
async fn generate_openapi_spec_from_module(
    project_root: &Path,
    app_entrypoint: &str,
    app_slug: &str,
) -> Result<(String, String), String> {
    debug!("generate_openapi_spec_from_module called:");
    debug!("  project_root: {}", project_root.display());
    debug!("  app_entrypoint: {}", app_entrypoint);
    debug!("  app_slug: {}", app_slug);

    let (module_path, attr_name) = app_entrypoint
        .split_once(':')
        .ok_or_else(|| "Invalid app-entrypoint format (expected 'module:attr')".to_string())?;

    let script = format!(
        r#"
import sys, json, importlib, os
project_root = sys.argv[1]
src = os.path.join(project_root, "src")
if os.path.isdir(src) and src not in sys.path:
    sys.path.insert(0, src)
if project_root not in sys.path:
    sys.path.insert(0, project_root)
mod = importlib.import_module("{module_path}")
app = getattr(mod, "{attr_name}")
print(json.dumps(app.openapi(), indent=2))
"#
    );

    let uv_path = try_resolve_uv()?.path;
    let output = tokio::process::Command::new(&uv_path)
        .args(["run", "--no-sync", "python", "-c", &script])
        .arg(project_root.to_string_lossy().as_ref())
        .current_dir(project_root)
        .output()
        .await
        .map_err(|e| format!("Failed to run uv for OpenAPI generation: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to generate OpenAPI schema: {stderr}"));
    }

    let spec_json = String::from_utf8(output.stdout)
        .map_err(|e| format!("OpenAPI output is not valid UTF-8: {e}"))?
        .trim()
        .to_string();

    Ok((spec_json, app_slug.to_string()))
}

/// Get the Databricks SDK version for a specific project directory via subprocess.
///
/// Uses `uv run --directory <project_dir>` to run in the project's venv context.
pub fn get_databricks_sdk_version_for_project(
    project_dir: &Path,
) -> Result<Option<String>, String> {
    debug!(
        "get_databricks_sdk_version_for_project: checking project at {}",
        project_dir.display()
    );

    let uv_path = match try_resolve_uv() {
        Ok(resolved) => resolved.path,
        Err(e) => {
            debug!("get_databricks_sdk_version_for_project: failed to resolve uv: {e}");
            return Ok(None);
        }
    };

    let dir_str = project_dir.to_str().unwrap_or(".");
    let output = Command::new(&uv_path)
        .args([
            "run",
            "--directory",
            dir_str,
            "--no-sync",
            "python",
            "-c",
            "import importlib.metadata; print(importlib.metadata.version('databricks-sdk'))",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if version.is_empty() {
                debug!("get_databricks_sdk_version_for_project: empty output");
                Ok(None)
            } else {
                debug!(
                    "get_databricks_sdk_version_for_project: found version {}",
                    version
                );
                Ok(Some(version))
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            debug!(
                "get_databricks_sdk_version_for_project: subprocess failed: {}",
                stderr
            );
            Ok(None)
        }
        Err(e) => {
            debug!(
                "get_databricks_sdk_version_for_project: failed to run uv: {}",
                e
            );
            Ok(None)
        }
    }
}

/// Get the installed Databricks SDK version via subprocess
pub fn get_databricks_sdk_version() -> Result<Option<String>, String> {
    debug!("get_databricks_sdk_version: Starting subprocess call");

    let uv_path = match try_resolve_uv() {
        Ok(resolved) => resolved.path,
        Err(e) => {
            debug!("get_databricks_sdk_version: failed to resolve uv: {e}");
            return Ok(None);
        }
    };
    let output = Command::new(&uv_path)
        .args([
            "run",
            "--no-sync",
            "python",
            "-c",
            "import importlib.metadata; print(importlib.metadata.version('databricks-sdk'))",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if version.is_empty() {
                debug!("get_databricks_sdk_version: empty output");
                Ok(None)
            } else {
                debug!("get_databricks_sdk_version: found version {}", version);
                Ok(Some(version))
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            debug!("get_databricks_sdk_version: subprocess failed: {}", stderr);
            Ok(None)
        }
        Err(e) => {
            debug!("get_databricks_sdk_version: failed to run uv: {}", e);
            Ok(None)
        }
    }
}
