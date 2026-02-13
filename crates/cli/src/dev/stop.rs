use clap::Args;
use std::path::PathBuf;

use crate::run_cli_async_helper;
use apx_core::ops::dev::stop_dev_server;

#[derive(Args, Debug, Clone)]
pub struct StopArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: StopArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: StopArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    stop_dev_server(&app_dir).await?;
    Ok(())
}
