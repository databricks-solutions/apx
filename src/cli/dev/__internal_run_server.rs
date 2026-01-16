use clap::Args;
use std::path::PathBuf;

use crate::cli::run_cli;
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

pub fn run(args: InternalRunServerArgs) -> i32 {
    run_cli(|| run_inner(args))
}

fn run_inner(args: InternalRunServerArgs) -> Result<(), String> {
    let backend_port =
        find_available_port_in_range(&args.host, BACKEND_PORT_START, BACKEND_PORT_END)?;
    let frontend_port =
        find_available_port_in_range(&args.host, FRONTEND_PORT_START, FRONTEND_PORT_END)?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| format!("Failed to start runtime: {err}"))?;
    runtime.block_on(run_server(
        args.app_dir,
        args.host,
        args.port,
        backend_port,
        frontend_port,
    ))
}
