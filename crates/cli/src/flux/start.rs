//! Start the flux OTEL collector daemon.

use clap::Args;
use std::time::Instant;

use crate::run_cli_async_helper;
use apx_core::common::{format_elapsed_ms, spinner};
use apx_core::flux;

#[derive(Args, Debug, Clone)]
pub struct StartArgs {}

pub async fn run(_args: StartArgs) -> i32 {
    run_cli_async_helper(run_inner).await
}

async fn run_inner() -> Result<(), String> {
    // Check if already running
    if flux::is_running() {
        println!(
            "✅ Flux already running at http://127.0.0.1:{}\n",
            flux::FLUX_PORT
        );
        return Ok(());
    }

    let start_time = Instant::now();
    let start_spinner = spinner("Starting flux daemon...");

    flux::start()?;

    start_spinner.finish_and_clear();
    println!(
        "✅ Flux started at http://127.0.0.1:{} in {}\n",
        flux::FLUX_PORT,
        format_elapsed_ms(start_time)
    );
    Ok(())
}
