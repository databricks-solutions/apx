use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

use crate::dev::common::{lock_path, read_lock};
use crate::external::uv::Uv;
use crate::resources;
use apx_common::hosts::CLIENT_HOST;

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

/// Write entrypoint.ts into the project's `node_modules/.apx/` and return its path.
pub fn ensure_frontend_entrypoint(project_root: &Path) -> Result<PathBuf, String> {
    resources::ensure_entrypoint(project_root)
}

/// Get the content of an embedded template file as a string.
///
/// The path is relative to `src/apx/templates/`, e.g. `"base/pyproject.toml.jinja2"`.
pub fn get_template_content(path: &str) -> Result<String, String> {
    resources::get_template_str(path)
        .map(|c| c.into_owned())
        .ok_or_else(|| format!("Template not found: {path}"))
}

/// List embedded template files matching a prefix.
///
/// Returns paths relative to the templates root, e.g. `["base/pyproject.toml.jinja2", ...]`.
pub fn list_template_files(prefix: &str) -> Vec<String> {
    resources::list_templates(Some(prefix))
}

/// Generate the OpenAPI JSON spec and its hash by invoking the Python app.
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

    let uv = Uv::try_new()?;
    let project_root_str = project_root.to_string_lossy();
    let spec_json = uv
        .run_python_code(project_root, &script, &[&project_root_str])
        .await?
        .into_stdout("uv")
        .map_err(|e| format!("Failed to generate OpenAPI schema: {e}"))?;

    Ok((spec_json, app_slug.to_string()))
}

/// Get the installed Databricks SDK version via subprocess.
///
/// When `project_dir` is `Some`, uses `uv run --directory <dir>` to run
/// in the project's venv context. When `None`, runs in the current context.
pub async fn get_databricks_sdk_version(
    project_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    let label = project_dir.map_or_else(|| "default".to_string(), |d| d.display().to_string());
    debug!("get_databricks_sdk_version: checking (context: {label})");

    let uv = match Uv::try_new() {
        Ok(uv) => uv,
        Err(e) => {
            debug!("get_databricks_sdk_version: failed to resolve uv: {e}");
            return Ok(None);
        }
    };

    let mut cmd = uv.cmd().arg("run");
    if let Some(dir) = project_dir {
        let dir_str = dir.to_str().unwrap_or(".");
        cmd = cmd.args(["--directory", dir_str]);
    }
    cmd = cmd.args([
        "--no-sync",
        "python",
        "-c",
        "import importlib.metadata; print(importlib.metadata.version('databricks-sdk'))",
    ]);

    match cmd.exec().await {
        Ok(output) if output.exit_code == Some(0) => {
            let version = output.stdout.trim().to_string();
            if version.is_empty() {
                debug!("get_databricks_sdk_version: empty output");
                Ok(None)
            } else {
                debug!("get_databricks_sdk_version: found version {version}");
                Ok(Some(version))
            }
        }
        Ok(output) => {
            debug!(
                "get_databricks_sdk_version: subprocess failed: {}",
                output.stderr
            );
            Ok(None)
        }
        Err(e) => {
            debug!("get_databricks_sdk_version: failed to run uv: {e}");
            Ok(None)
        }
    }
}
