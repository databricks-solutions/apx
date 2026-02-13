#![forbid(unsafe_code)]
#![deny(warnings, unused_must_use, dead_code, missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

#[pyfunction]
fn run_cli(args: Vec<String>) -> i32 {
    apx_cli::run_cli(args)
}

#[pyfunction]
fn get_bun_binary_path(py: Python<'_>) -> PyResult<Py<PyAny>> {
    apx_core::interop::get_bun_binary_path(py)
}

#[pyfunction(name = "generate_openapi")]
fn generate_openapi_py(project_root: PathBuf) -> PyResult<()> {
    apx_core::api_generator::generate_openapi(&project_root).map_err(PyRuntimeError::new_err)
}

#[pyfunction]
fn get_dotenv_vars() -> PyResult<HashMap<String, String>> {
    use tracing::warn;

    let app_dir = apx_core::app_state::get_app_dir()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| PyRuntimeError::new_err("Failed to determine app directory"))?;

    let dotenv_path = app_dir.join(".env");

    if !dotenv_path.exists() {
        warn!(
            ".env file not found at {}, using empty environment",
            dotenv_path.display()
        );
        return Ok(HashMap::new());
    }

    let dotenv =
        apx_core::dotenv::DotenvFile::read(&dotenv_path).map_err(PyRuntimeError::new_err)?;
    Ok(dotenv.get_vars())
}

/// A Python module implemented in Rust. The name of this module must match
/// the `lib.name` setting in the `Cargo.toml`, else Python will not be able to
/// import the module.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    apx_core::tracing_init::init_tracing();
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(run_cli, m)?)?;
    m.add_function(wrap_pyfunction!(get_bun_binary_path, m)?)?;
    m.add_function(wrap_pyfunction!(generate_openapi_py, m)?)?;
    m.add_function(wrap_pyfunction!(get_dotenv_vars, m)?)?;
    Ok(())
}
