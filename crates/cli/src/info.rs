use clap::Args;

use apx_core::external::bun::Bun;
use apx_core::external::databricks::DatabricksCli;
use apx_core::external::gh::Gh;
use apx_core::external::git::Git;
use apx_core::external::uv::Uv;
use apx_core::external::{ToolInfo, ToolInfoEntry};

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

// Reason: direct stdout is required for info display
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
        .map_or_else(
            || "unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        );

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    println!();
    println!("{BOLD}{YELLOW}\u{1f4e6} apx{RESET}");
    println!("   {DIM}Version:{RESET}  {GREEN}{version}{RESET}");
    println!("   {DIM}Build:{RESET}    {build_time} ({git_hash})");
    println!("   {DIM}OS:{RESET}       {os} {arch}");

    // --- external tools ---
    let (uv, bun, git, gh, databricks) = tokio::join!(
        Uv::info(),
        Bun::info(),
        Git::info(),
        Gh::info(),
        DatabricksCli::info(),
    );

    print_tool_entry(&uv);
    print_tool_entry(&bun);
    print_tool_entry(&git);
    print_tool_entry(&gh);
    print_tool_entry(&databricks);

    println!();
    Ok(())
}

// Reason: direct stdout is required for info display
#[allow(clippy::print_stdout)]
fn print_tool_entry(entry: &ToolInfoEntry) {
    println!();
    println!("{BOLD}{YELLOW}{} {}{RESET}", entry.emoji, entry.name);
    if let Some(ref err) = entry.error {
        println!("   {RED}{err}{RESET}");
    } else {
        if let Some(ref ver) = entry.version {
            println!("   {DIM}Version:{RESET}  {GREEN}{ver}{RESET}");
        }
        if let Some(ref path) = entry.path {
            println!("   {DIM}Path:{RESET}     {CYAN}{path}{RESET}");
        }
        if let Some(ref source) = entry.source {
            println!("   {DIM}Source:{RESET}   {source}");
        }
    }
}
