use crate::run_cli_async_helper;
use apx_core::external::bun::Bun;
use clap::Args;
use tokio::select;
use tokio::signal;
use tracing::debug;

#[derive(Args, Debug, Clone)]
pub struct BunArgs {
    /// Arguments passed directly to `bun`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn run(args: BunArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

pub async fn run_inner(args: BunArgs) -> Result<(), String> {
    let bun = Bun::new().await?;

    debug!(
        args = ?args.args,
        "Running bun with passthrough args"
    );

    let mut child = bun
        .passthrough(&args.args)
        .map_err(|e| format!("Failed to spawn bun: {e}"))?;

    select! {
        status = child.wait() => {
            let status = status.map_err(|e| format!("Failed to wait for bun: {e}"))?;

            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "bun exited with status {}",
                    status.code().unwrap_or(1)
                ))
            }
        }

        _ = signal::ctrl_c() => {
            debug!("Ctrl+C received, stopping bun");
            let _ = child.kill().await;
            Ok(())
        }
    }
}
