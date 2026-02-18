use clap::Args;
use std::path::PathBuf;

use apx_core::api_generator::generate_openapi;

#[derive(Args, Debug, Clone)]
pub struct GenerateOpenapiArgs {
    #[arg(long = "app-dir", value_name = "APP_PATH")]
    pub app_dir: PathBuf,
}

pub async fn run(args: GenerateOpenapiArgs) -> i32 {
    match generate_openapi(&args.app_dir).await {
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
