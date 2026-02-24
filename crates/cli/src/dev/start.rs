use clap::Args;
use std::path::PathBuf;

use crate::common::find_app_dir;
use crate::run_cli_async_helper;
use apx_core::common::OutputMode;
use apx_core::ops::dev::stop_dev_server;
use apx_core::ops::dev::{ServerLauncher, prepare_server_launch, resolve_existing_server};

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
    #[arg(
        long = "skip-healthcheck",
        help = "Skip waiting for the dev server to become healthy before returning"
    )]
    pub skip_healthcheck: bool,
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
    let app_dir = find_app_dir(args.app_path)?;
    let mode = OutputMode::Interactive;

    // Check for existing server first
    if let Some(port) = resolve_existing_server(&app_dir, mode).await? {
        apx_core::common::emit(
            mode,
            &format!(
                "Dev server is already running at http://{}:{port}\n",
                apx_common::hosts::BROWSER_HOST
            ),
        );
        return Ok(());
    }

    let server = prepare_server_launch(&app_dir, None, mode).await?;
    let launcher = ServerLauncher::Detached {
        app_dir: app_dir.clone(),
        skip_credentials_validation: args.skip_credentials_validation,
        timeout_secs: args.timeout,
        skip_healthcheck: args.skip_healthcheck,
        mode,
    };
    launcher.launch(server).await?;
    Ok(())
}

async fn run_attached(args: StartArgs) -> Result<(), String> {
    let app_dir = find_app_dir(args.app_path)?;
    let mode = OutputMode::Interactive;

    // If detached server already running, fall back to log tailing
    if resolve_existing_server(&app_dir, mode).await?.is_some() {
        let logs_args = super::logs::LogsArgs {
            app_path: Some(app_dir.clone()),
            duration: "10m".to_string(),
            follow: true,
        };
        let _ = super::logs::run(logs_args).await;
        stop_dev_server(&app_dir, mode).await?;
        return Ok(());
    }

    let server = prepare_server_launch(&app_dir, None, mode).await?;
    let launcher = ServerLauncher::Attached {
        app_dir: app_dir.clone(),
        skip_credentials_validation: args.skip_credentials_validation,
    };
    launcher.launch(server).await?;
    Ok(())
}
