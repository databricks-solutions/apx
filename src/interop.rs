use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

#[cfg(target_os = "windows")]
const BUN_FILENAME: &str = "bun.exe";
#[cfg(not(target_os = "windows"))]
const BUN_FILENAME: &str = "bun";

pub(crate) fn get_bun_binary_path(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let bun_path = resolve_bun_binary_path(py)?;
    let pathlib = py.import("pathlib")?;
    let path_cls = pathlib.getattr("Path")?;
    let path_obj = path_cls.call1((bun_path.to_string_lossy().as_ref(),))?;
    Ok(path_obj.unbind())
}

pub(crate) fn bun_binary_path() -> Result<PathBuf, String> {
    Python::attach(|py| {
        resolve_bun_binary_path(py)
            .map_err(|err| format!("Failed to resolve bun binary path: {err}"))
    })
}

fn resolve_bun_binary_path(py: Python<'_>) -> PyResult<PathBuf> {
    let importlib = py.import("importlib.resources")?;
    let files = importlib.getattr("files")?;
    let apx_resources = files.call1(("apx",))?;
    let binaries_dir = apx_resources.getattr("joinpath")?.call1(("binaries",))?;
    let bun_path = binaries_dir.getattr("joinpath")?.call1((BUN_FILENAME,))?;
    let fspath = bun_path.getattr("__fspath__")?.call0()?;
    let bun_path_str: String = fspath.extract()?;
    Ok(PathBuf::from(bun_path_str))
}

/// Get the path to the frontend entrypoint.ts asset
pub(crate) fn frontend_entrypoint_path() -> Result<PathBuf, String> {
    Python::attach(|py| {
        resolve_frontend_entrypoint_path(py)
            .map_err(|err| format!("Failed to resolve frontend entrypoint path: {err}"))
    })
}

fn resolve_frontend_entrypoint_path(py: Python<'_>) -> PyResult<PathBuf> {
    let importlib = py.import("importlib.resources")?;
    let files = importlib.getattr("files")?;
    let apx_resources = files.call1(("apx",))?;
    let assets_dir = apx_resources.getattr("joinpath")?.call1(("assets",))?;
    let entrypoint_path = assets_dir.getattr("joinpath")?.call1(("entrypoint.ts",))?;
    let fspath = entrypoint_path.getattr("__fspath__")?.call0()?;
    let entrypoint_path_str: String = fspath.extract()?;
    Ok(PathBuf::from(entrypoint_path_str))
}

pub(crate) fn list_profiles() -> Result<Vec<String>, String> {
    Python::attach(|py| {
        let utils = py
            .import("apx.utils")
            .map_err(|err| format!("Failed to import apx.utils: {err}"))?;
        let func = utils
            .getattr("list_profiles")
            .map_err(|err| format!("Failed to access list_profiles: {err}"))?;
        let profiles = func
            .call0()
            .map_err(|err| format!("Failed to call list_profiles: {err}"))?;
        profiles
            .extract::<Vec<String>>()
            .map_err(|err| format!("Failed to parse profiles: {err}"))
    })
}

pub(crate) fn templates_dir() -> Result<PathBuf, String> {
    Python::attach(|py| {
        let importlib = py
            .import("importlib.resources")
            .map_err(|err| format!("Failed to import importlib.resources: {err}"))?;
        let files = importlib
            .getattr("files")
            .map_err(|err| format!("Failed to access importlib.resources.files: {err}"))?;
        let apx_resources = files
            .call1(("apx",))
            .map_err(|err| format!("Failed to access apx package resources: {err}"))?;
        let templates_dir = apx_resources
            .getattr("joinpath")
            .map_err(|err| format!("Failed to access joinpath: {err}"))?
            .call1(("templates",))
            .map_err(|err| format!("Failed to resolve templates path: {err}"))?;
        let fspath = templates_dir
            .getattr("__fspath__")
            .map_err(|err| format!("Failed to access __fspath__: {err}"))?
            .call0()
            .map_err(|err| format!("Failed to resolve templates path: {err}"))?;
        let templates_path: String = fspath
            .extract()
            .map_err(|err| format!("Failed to parse templates path: {err}"))?;
        Ok(PathBuf::from(templates_path))
    })
}

pub(crate) fn validate_credentials() -> Result<(), String> {
    Python::attach(|py| -> PyResult<()> {
        let interop = py.import("apx.interop")?;
        let result = interop.call_method0("credentials_valid")?;
        let (valid, error): (bool, String) = result.extract()?;
        if !valid {
            return Err(PyRuntimeError::new_err(error));
        }
        Ok(())
    })
    .map_err(|e| format!("Credentials validation failed: {e}"))
}

pub(crate) fn get_token() -> Result<String, String> {
    Python::attach(|py| {
        let interop = py
            .import("apx.interop")
            .map_err(|e| format!("Failed to import apx.interop: {e}"))?;
        let token: String = interop
            .call_method0("get_token")
            .map_err(|e| format!("Failed to call get_token: {e}"))?
            .extract()
            .map_err(|e| format!("Failed to extract token: {e}"))?;
        Ok(token)
    })
}

pub(crate) fn get_forwarded_user_header() -> Result<String, String> {
    Python::attach(|py| {
        let interop = py
            .import("apx.interop")
            .map_err(|e| format!("Failed to import apx.interop: {e}"))?;
        let header_value: String = interop
            .call_method0("get_forwarded_user_header")
            .map_err(|e| format!("Failed to call get_forwarded_user_header: {e}"))?
            .extract()
            .map_err(|e| format!("Failed to extract forwarded user header: {e}"))?;
        Ok(header_value)
    })
}

pub(crate) fn generate_openapi_spec(
    project_root: &Path,
    app_module: &str,
    app_slug: &str,
) -> Result<(String, String), String> {
    let project_root_str = project_root.to_string_lossy().to_string();
    let src_root = project_root.join("src");
    let src_root_str = src_root.to_string_lossy().to_string();

    debug!("generate_openapi_spec called:");
    debug!("  project_root: {}", project_root_str);
    debug!("  src_root: {}", src_root_str);
    debug!("  app_module: {}", app_module);
    debug!("  app_slug: {}", app_slug);
    debug!("  src_root exists: {}", src_root.exists());

    Python::attach(|py| -> PyResult<(String, String)> {
        let sys = py.import("sys")?;
        let path_any = sys.getattr("path")?;
        let path = path_any.cast::<PyList>()?;
        
        debug!("sys.path before modifications: {:?}", path.extract::<Vec<String>>());
        
        if src_root.exists() && !path.contains(src_root_str.as_str())? {
            path.insert(0, src_root_str.as_str())?;
            debug!("Added src_root to sys.path: {}", src_root_str);
        } else {
            debug!("src_root NOT added (exists: {}, already in path: {})", 
                src_root.exists(), 
                path.contains(src_root_str.as_str()).unwrap_or(false)
            );
        }
        
        if !path.contains(project_root_str.as_str())? {
            path.insert(0, project_root_str.as_str())?;
            debug!("Added project_root to sys.path: {}", project_root_str);
        } else {
            debug!("project_root already in sys.path");
        }
        
        debug!("sys.path after modifications: {:?}", path.extract::<Vec<String>>());

        let (module_path, attr_name) = app_module
            .split_once(':')
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Invalid app-module format"))?;

        debug!("Attempting to import module: {} (attr: {})", module_path, attr_name);
        
        let importlib = py.import("importlib")?;
        let module = importlib.call_method1("import_module", (module_path,))?;
        debug!("Successfully imported module: {}", module_path);
        
        let app = module.getattr(attr_name)?;
        debug!("Successfully got attribute: {}", attr_name);
        
        let spec = app.call_method0("openapi")?;
        debug!("Successfully generated OpenAPI spec");
        
        let json = py.import("json")?;
        let dumps_kwargs = PyDict::new(py);
        dumps_kwargs.set_item("indent", 2)?;
        let spec_json: String = json
            .call_method("dumps", (spec,), Some(&dumps_kwargs))?
            .extract()?;

        Ok((spec_json, app_slug.to_string()))
    })
    .map_err(|err| format!("Failed to generate OpenAPI schema: {err}"))
    .map(|(spec_json, app_slug)| {
        // Check for missing operation_id in routes
        if let Ok(spec_value) = serde_json::from_str::<serde_json::Value>(&spec_json) {
            if let Some(paths) = spec_value.get("paths").and_then(|p| p.as_object()) {
                for (path, path_value) in paths {
                    if let Some(path_obj) = path_value.as_object() {
                        for (method, operation_value) in path_obj {
                            if let Some(operation_obj) = operation_value.as_object() {
                                if !operation_obj.contains_key("operation_id") 
                                    && !operation_obj.contains_key("operationId") {
                                    warn!(
                                        path = %path,
                                        method = %method,
                                        "Route is missing operation_id property"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        (spec_json, app_slug)
    })
}

/// Get the installed Databricks SDK version
pub(crate) fn get_databricks_sdk_version() -> Result<Option<String>, String> {
    Python::attach(|py| {
        // Try to import databricks.sdk and get its version
        let importlib = py.import("importlib.metadata");

        match importlib {
            Ok(module) => {
                let version_fn = module
                    .getattr("version")
                    .map_err(|e| format!("Failed to get version function: {e}"))?;

                let version = version_fn.call1(("databricks-sdk",));

                match version {
                    Ok(v) => {
                        let version_str: String = v
                            .extract()
                            .map_err(|e| format!("Failed to extract version string: {e}"))?;
                        Ok(Some(version_str))
                    }
                    Err(_) => {
                        // databricks-sdk not installed
                        Ok(None)
                    }
                }
            }
            Err(_) => {
                // importlib.metadata not available
                Ok(None)
            }
        }
    })
}
