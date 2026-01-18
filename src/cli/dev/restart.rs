use clap::Args;
use std::path::PathBuf;

use crate::cli::dev::start::start_server;
use crate::cli::dev::stop::stop_server_inner;
use crate::cli::run_cli;
use crate::dev::common::{lock_path, read_lock, CLIENT_HOST};

#[derive(Args, Debug, Clone)]
pub struct RestartArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub fn run(args: RestartArgs) -> i32 {
    run_cli(|| run_inner(args))
}

fn run_inner(args: RestartArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    
    let lock_path = lock_path(&app_dir);
    let preferred_port = if lock_path.exists() {
        let lock = read_lock(&lock_path)?;
        println!(
            "Found existing dev server at http://{CLIENT_HOST}:{port}",
            port = lock.port
        );
        Some(lock.port)
    } else {
        println!("No existing dev server found, starting fresh...");
        None
    };
    
    println!("Stopping dev server...");
    stop_server_inner(&app_dir)?;
    println!("Starting dev server...");
    let port = start_server(&app_dir, preferred_port)?;
    println!(
        "Dev server restarted at http://{CLIENT_HOST}:{port}",
        port = port
    );
    Ok(())
}
