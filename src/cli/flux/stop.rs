//! Stop the flux OTEL collector daemon.

use clap::Args;
use std::time::Instant;

use crate::cli::run_cli_async;
use crate::common::{format_elapsed_ms, spinner};
use crate::flux;

#[derive(Args, Debug, Clone)]
pub struct StopArgs {}

pub async fn run(_args: StopArgs) -> i32 {
    run_cli_async(|| run_inner()).await
}

async fn run_inner() -> Result<(), String> {
    if !flux::is_running() {
        println!("⚠️  Flux is not running\n");
        return Ok(());
    }

    let start_time = Instant::now();
    let stop_spinner = spinner("Stopping flux daemon...");

    flux::stop()?;

    stop_spinner.finish_and_clear();
    println!("✅ Flux stopped in {}\n", format_elapsed_ms(start_time));
    Ok(())
}
