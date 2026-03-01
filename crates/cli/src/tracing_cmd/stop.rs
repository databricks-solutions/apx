//! Stop the tracing OTEL collector daemon.

use clap::Args;
use std::time::Instant;

use crate::run_cli_async_helper;
use apx_core::collector;
use apx_core::common::{format_elapsed_ms, spinner};

#[derive(Args, Debug, Clone)]
pub struct StopArgs {}

pub async fn run(_args: StopArgs) -> i32 {
    run_cli_async_helper(run_inner).await
}

async fn run_inner() -> Result<(), String> {
    if !collector::is_running() {
        println!("⚠️  Tracing collector is not running\n");
        return Ok(());
    }

    let start_time = Instant::now();
    let stop_spinner = spinner("Stopping tracing collector...");

    collector::stop()?;

    stop_spinner.finish_and_clear();
    println!(
        "✅ Tracing collector stopped in {}\n",
        format_elapsed_ms(start_time)
    );
    Ok(())
}
