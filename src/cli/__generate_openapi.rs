use clap::Args;
use std::path::PathBuf;

use crate::generate_openapi;

#[derive(Args, Debug, Clone)]
pub struct GenerateOpenapiArgs {
    #[arg(long = "app-dir", value_name = "APP_PATH")]
    pub app_dir: PathBuf,
}

pub fn run(args: GenerateOpenapiArgs) -> i32 {
    match generate_openapi(&args.app_dir) {
        Ok(()) => {
            println!("regenerated");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
