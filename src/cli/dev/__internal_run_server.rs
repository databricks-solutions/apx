use clap::Args;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::PathBuf;

use crate::cli::run_cli_async;
use crate::set_app_dir;
use crate::dev::common::{
    find_available_port_in_range, BACKEND_PORT_END, BACKEND_PORT_START, BIND_HOST,
    FRONTEND_PORT_END, FRONTEND_PORT_START,
};
use crate::dev::server::run_server;

#[derive(Args, Debug, Clone)]
pub struct InternalRunServerArgs {
    #[arg(long = "app-dir", value_name = "APP_PATH")]
    pub app_dir: PathBuf,
    #[arg(long = "host", default_value = BIND_HOST)]
    pub host: String,
    #[arg(long = "port")]
    pub port: u16,
}

pub async fn run(args: InternalRunServerArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: InternalRunServerArgs) -> Result<(), String> {
    set_app_dir(args.app_dir.clone())?;
    
    // Load dotenv and validate credentials before starting server
    validate_credentials()?;
    
    let backend_port =
        find_available_port_in_range(&args.host, BACKEND_PORT_START, BACKEND_PORT_END)?;
    let frontend_port =
        find_available_port_in_range(&args.host, FRONTEND_PORT_START, FRONTEND_PORT_END)?;
    run_server(
        args.app_dir,
        args.host,
        args.port,
        backend_port,
        frontend_port,
    )
    .await
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
    }).map_err(|e| format!("Credentials validation failed: {e}"))
}
