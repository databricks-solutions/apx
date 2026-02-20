use clap::Args;
use std::path::PathBuf;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::OutputMode;
use apx_core::ops::dev::restart_dev_server;

#[derive(Args, Debug, Clone)]
pub struct RestartArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        long = "skip-healthcheck",
        help = "Skip waiting for the dev server to become healthy before returning"
    )]
    pub skip_healthcheck: bool,
}

pub async fn run(args: RestartArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: RestartArgs) -> Result<(), String> {
    let app_dir = find_app_dir(args.app_path)?;

    restart_dev_server(&app_dir, args.skip_healthcheck, OutputMode::Interactive).await?;
    Ok(())
}
