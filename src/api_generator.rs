use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::fs;
use std::path::Path;
use std::process::Command;

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