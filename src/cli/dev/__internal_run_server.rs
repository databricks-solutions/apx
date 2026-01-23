use clap::Args;
use std::path::PathBuf;
use tracing::warn;

use crate::cli::run_cli_async;
use crate::set_app_dir;
use crate::dev::common::{
    find_available_port_in_range, BACKEND_PORT_END, BACKEND_PORT_START, BIND_HOST,
    DB_PORT_END, DB_PORT_START, FRONTEND_PORT_END, FRONTEND_PORT_START,
};
use crate::dev::server::run_server;
use crate::interop::validate_credentials;

#[derive(Args, Debug, Clone)]
pub struct InternalRunServerArgs {
    #[arg(long = "app-dir", value_name = "APP_PATH")]
    pub app_dir: PathBuf,
    #[arg(long = "host", default_value = BIND_HOST)]
    pub host: String,
    #[arg(long = "port")]
    pub port: u16,
    #[arg(long = "skip-credentials-validation")]
    pub skip_credentials_validation: bool,
}

pub async fn run(args: InternalRunServerArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: InternalRunServerArgs) -> Result<(), String> {
    set_app_dir(args.app_dir.clone())?;
    
    // Validate credentials before starting server (warn if skipped or failed)
    if args.skip_credentials_validation {
        warn!("Credentials validation skipped. API proxy may not work correctly.");
    } else if let Err(err) = validate_credentials() {
        warn!("Credentials validation failed: {err}. API proxy may not work correctly.");
    }
    
    let backend_port =
        find_available_port_in_range(&args.host, BACKEND_PORT_START, BACKEND_PORT_END)?;
    let frontend_port =
        find_available_port_in_range(&args.host, FRONTEND_PORT_START, FRONTEND_PORT_END)?;
    let db_port =
        find_available_port_in_range(&args.host, DB_PORT_START, DB_PORT_END)?;
    run_server(
        args.app_dir,
        args.host,
        args.port,
        backend_port,
        frontend_port,
        db_port,
    )
    .await
}
