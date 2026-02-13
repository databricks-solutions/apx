use clap::Args;
use std::path::PathBuf;

use crate::common::resolve_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::OutputMode;
use apx_core::ops::check::run_check;

#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: CheckArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: CheckArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path);

    run_check(&app_dir, OutputMode::Interactive).await
}
