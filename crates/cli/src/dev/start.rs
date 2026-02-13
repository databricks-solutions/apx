use clap::Args;
use std::path::PathBuf;

use crate::common::resolve_app_dir;
use crate::run_cli_async_helper;
use apx_core::ops::dev::stop_dev_server;
use apx_core::ops::dev::{spawn_server, start_dev_server};

#[derive(Args, Debug, Clone)]
pub struct StartArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        short = 'a',
        long = "attached",
        help = "Follow logs and stop server on Ctrl+C"
    )]
    pub attached: bool,
    #[arg(
        long = "skip-credentials-validation",
        help = "Skip credentials validation on startup (server will start but API proxy may not work)"
    )]
    pub skip_credentials_validation: bool,
    #[arg(
        long = "timeout",
        default_value = "60",
        value_name = "SECONDS",
        help = "Maximum time in seconds to wait for dev server to become healthy"
    )]
    pub timeout: u64,
}

pub async fn run(args: StartArgs) -> i32 {
    run_cli_async_helper(|| async {
        if args.attached {
            run_attached(args).await
        } else {
            run_detached(args).await
        }
    })
    .await
}

async fn run_detached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path);
    let _ = start_dev_server(&app_dir).await?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path);

    let _port = spawn_server(
        &app_dir,
        None,
        args.skip_credentials_validation,
        args.timeout,
    )
    .await?;

    // Use the SQLite-based log following (reads from flux storage)
    let logs_args = super::logs::LogsArgs {
        app_path: Some(app_dir.clone()),
        duration: "10m".to_string(),
        follow: true,
    };

    // Run logs command (will return on Ctrl+C)
    let _ = super::logs::run(logs_args).await;

    stop_dev_server(&app_dir).await?;
    Ok(())
}
