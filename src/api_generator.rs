use notify::{RecursiveMode, Watcher};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Sleep};
use tracing::{info, warn};

use crate::bun_binary_path;
use crate::common::read_project_metadata;

const APX_DIR_NAME: &str = ".apx";
const SCHEMA_FILENAME: &str = "openapi.json";
const ORVAL_CONFIG_FILENAME: &str = "orval.config.ts";
const ORVAL_SCHEMA_INPUT: &str = ".apx/openapi.json";

pub fn generate_openapi(project_root: &Path, force: bool) -> Result<bool, String> {
    let project_root_str = project_root.to_string_lossy().to_string();
    let metadata = read_project_metadata(project_root)?;
    let app_slug = metadata.app_slug;
    let app_module = metadata.app_module;

    let (spec_json, app_slug) = Python::attach(|py| -> PyResult<(String, String)> {
        let sys = py.import("sys")?;
        let path_any = sys.getattr("path")?;
        let path = path_any.cast::<PyList>()?;
        if !path.contains(project_root_str.as_str())? {
            path.insert(0, project_root_str.as_str())?;
        }

        let (module_path, attr_name) = app_module
            .split_once(':')
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Invalid app-module format"))?;
        let importlib = py.import("importlib")?;
        let module = importlib.call_method1("import_module", (module_path,))?;
        let module = importlib.call_method1("reload", (module,))?;
        let app = module.getattr(attr_name)?;
        let spec = app.call_method0("openapi")?;
        let json = py.import("json")?;
        let dumps_kwargs = PyDict::new(py);
        dumps_kwargs.set_item("indent", 2)?;
        let spec_json: String = json.call_method("dumps", (spec,), Some(&dumps_kwargs))?.extract()?;

        Ok((spec_json, app_slug.clone()))
    })
    .map_err(|err| format!("Failed to generate OpenAPI schema: {err}"))?;

    let apx_dir = project_root.join(APX_DIR_NAME);
    let schema_path = apx_dir.join(SCHEMA_FILENAME);
    let config_path = apx_dir.join(ORVAL_CONFIG_FILENAME);

    fs::create_dir_all(&apx_dir)
        .map_err(|err| format!("Failed to create .apx directory: {err}"))?;

    let mut schema_changed = true;
    if schema_path.exists() {
        let existing = fs::read_to_string(&schema_path)
            .map_err(|err| format!("Failed to read existing schema: {err}"))?;
        if existing == spec_json {
            schema_changed = false;
        }
    }

    if schema_changed {
        fs::write(&schema_path, &spec_json)
            .map_err(|err| format!("Failed to write OpenAPI schema: {err}"))?;
    }

    if !config_path.exists() {
        let config_content = format!(
            r#"import {{ defineConfig }} from "orval";

export default defineConfig({{
  api: {{
    input: ".apx/openapi.json",
    output: {{
      target: "../src/{app_slug}/ui/lib/api.ts",
      client: "react-query",
      httpClient: "axios",
      prettier: true,
      override: {{
        query: {{
          useQuery: true,
          useSuspenseQuery: true,
        }},
      }},
    }},
  }},
}});
"#,
            app_slug = app_slug
        );
        fs::write(&config_path, config_content)
            .map_err(|err| format!("Failed to write orval config: {err}"))?;
    }

    if !schema_changed && !force {
        return Ok(false);
    }

    let bun_path = bun_binary_path()?;
    let output = Command::new(bun_path)
        .arg("x")
        .arg("--bun")
        .arg("orval")
        .arg("-i")
        .arg(ORVAL_SCHEMA_INPUT)
        .arg("-c")
        .arg(config_path.to_string_lossy().as_ref())
        .current_dir(project_root)
        .output()
        .map_err(|err| format!("Failed to run orval: {err}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Orval failed with status {status}. Stdout: {stdout} Stderr: {stderr}",
            status = output.status
        ));
    }

    Ok(true)
}

const OPENAPI_WATCH_DEBOUNCE_MS: u64 = 300;

pub fn start_openapi_watcher(
    app_dir: PathBuf,
    is_stopping: Arc<AtomicBool>,
) -> Result<(), String> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .map_err(|err| format!("Failed to create file watcher: {err}"))?;
    watcher
        .watch(&app_dir, RecursiveMode::Recursive)
        .map_err(|err| format!("Failed to watch app dir: {err}"))?;

    tokio::spawn(async move {
        let _watcher = watcher;
        let mut pending = false;
        let mut debounce: Option<Pin<Box<Sleep>>> = None;

        loop {
            if is_stopping.load(Ordering::SeqCst) {
                break;
            }

            tokio::select! {
                maybe = rx.recv() => {
                    let Some(result) = maybe else { break; };
                    match result {
                        Ok(event) => {
                            if event
                                .paths
                                .iter()
                                .any(|path| !is_ignored_path(path) && is_python_path(path))
                            {
                                pending = true;
                                debounce = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                                    OPENAPI_WATCH_DEBOUNCE_MS,
                                ))));
                            }
                        }
                        Err(err) => {
                            warn!("OpenAPI watcher error: {err}");
                        }
                    }
                }
                _ = debounce.as_mut().unwrap(), if debounce.is_some() => {
                    debounce = None;
                    if pending {
                        pending = false;
                        info!("Python change detected, regenerating OpenAPI…");
                        let app_dir = app_dir.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            generate_openapi(&app_dir, false)
                        })
                        .await;
                        match result {
                            Ok(Ok(true)) => info!("OpenAPI regenerated"),
                            Ok(Ok(false)) => info!("OpenAPI unchanged, skipped"),
                            Ok(Err(err)) => warn!("OpenAPI regeneration failed: {err}"),
                            Err(err) => warn!("OpenAPI regeneration task failed: {err}"),
                        }
                    }
                }
            }
        }
    });

    Ok(())
}

fn is_python_path(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "py")
}

fn is_ignored_path(path: &PathBuf) -> bool {
    const IGNORED_DIRS: [&str; 11] = [
        ".git",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".tox",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "venv",
    ];

    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let Some(name) = name.to_str() else {
            return false;
        };
        IGNORED_DIRS.iter().any(|ignored| ignored == &name)
    })
}