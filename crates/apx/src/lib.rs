// PyO3 #[pymodule] and #[pyfunction] macros generate `unsafe impl` blocks.
#![allow(unsafe_code)]

//! `apx._core` — the PyO3 extension module.
//!
//! This is a `cdylib` that ships inside the `apx` Python wheel. It exposes:
//! - `run_cli(args)` — the Rust CLI, called from the `apx` script entrypoint
//! - Framework types (`HttpMethod`, `Request`, `Response`, etc.) via [`apx_framework::pyapi`]

use pyo3::prelude::*;

/// The `apx._core` extension module.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_cli, m)?)?;
    apx_framework::pyapi::register(m)?;
    Ok(())
}

/// Run the apx CLI with the given arguments.
///
/// Called from the Python script entrypoint (`apx:_main`).
/// Returns the process exit code.
#[pyfunction]
fn run_cli(py: Python<'_>, args: Vec<String>) -> i32 {
    py.detach(|| apx_cli::run_cli(args))
}
