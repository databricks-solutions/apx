//! Internal command to run the flux daemon server.
//! This is spawned by `flux start` and should not be called directly.

use clap::Args;

use crate::cli::run_cli_async;
use crate::flux;

#[derive(Args, Debug, Clone)]
pub struct RunArgs {}

pub async fn run(_args: RunArgs) -> i32 {
    run_cli_async(|| flux::run_server()).await
}
