//! Start the tracing OTEL collector daemon.

use clap::Args;
use std::time::Instant;

use crate::run_cli_async_helper;
use apx_core::collector;
use apx_core::common::{format_elapsed_ms, spinner};

#[derive(Args, Debug, Clone)]
pub struct StartArgs {}

pub async fn run(_args: StartArgs) -> i32 {
    run_cli_async_helper(run_inner).await
}

async fn run_inner() -> Result<(), String> {
    // Check if already running
    if collector::is_running() {
        println!(
            "✅ Tracing collector already running at http://127.0.0.1:{}\n",
            collector::COLLECTOR_PORT
        );
        return Ok(());
    }

    let start_time = Instant::now();
    let start_spinner = spinner("Starting tracing collector...");

    collector::start()?;

    start_spinner.finish_and_clear();
    println!(
        "✅ Tracing collector started at http://127.0.0.1:{} in {}\n",
        collector::COLLECTOR_PORT,
        format_elapsed_ms(start_time)
    );
    Ok(())
}
