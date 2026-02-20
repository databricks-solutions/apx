use clap::Args;

use apx_core::common::{BunCommand, UvCommand};

use crate::run_cli_async_helper;

// ANSI color codes
const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Args)]
pub struct InfoArgs {}

pub async fn run(_args: InfoArgs) -> i32 {
    run_cli_async_helper(run_inner).await
}

#[allow(clippy::print_stdout)]
async fn run_inner() -> Result<(), String> {
    // --- apx section ---
    let version = env!("CARGO_PKG_VERSION");
    let git_hash = env!("GIT_HASH");
    let build_ts = env!("BUILD_TIMESTAMP");
    let build_time = build_ts
        .parse::<i64>()
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    println!();
    println!("{BOLD}{YELLOW}\u{1f4e6} apx{RESET}");
    println!("   {DIM}Version:{RESET}  {GREEN}{version}{RESET}");
    println!("   {DIM}Build:{RESET}    {build_time} ({git_hash})");
    println!("   {DIM}OS:{RESET}       {os} {arch}");

    // --- uv section ---
    println!();
    println!("{BOLD}{YELLOW}\u{1f40d} uv{RESET}");
    match UvCommand::new("uv").await {
        Ok(uv) => {
            let ver = get_version(uv.path()).await;
            println!("   {DIM}Version:{RESET}  {GREEN}{ver}{RESET}");
            println!(
                "   {DIM}Path:{RESET}     {CYAN}{}{RESET}",
                uv.path().display()
            );
            println!("   {DIM}Source:{RESET}   {}", uv.source().source_label());
        }
        Err(e) => {
            println!("   {RED}{e}{RESET}");
        }
    }

    // --- bun section ---
    println!();
    println!("{BOLD}{YELLOW}\u{1f35e} bun{RESET}");
    match BunCommand::new().await {
        Ok(bun) => {
            let ver = get_version(bun.path()).await;
            println!("   {DIM}Version:{RESET}  {GREEN}{ver}{RESET}");
            println!(
                "   {DIM}Path:{RESET}     {CYAN}{}{RESET}",
                bun.path().display()
            );
            println!("   {DIM}Source:{RESET}   {}", bun.source().source_label());
        }
        Err(e) => {
            println!("   {RED}{e}{RESET}");
        }
    }

    println!();
    Ok(())
}

async fn get_version(binary: &std::path::Path) -> String {
    tokio::process::Command::new(binary)
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
