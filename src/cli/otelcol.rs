use crate::interop::otelcol_binary_path;
use crate::cli::run_cli_async;
use clap::Args;
use tokio::process::Command as TokioCommand;
use tracing::debug;
use tokio::signal;
use tokio::select;

#[derive(Args, Debug, Clone)]
pub struct OtelcolArgs {
    /// Arguments passed directly to `otelcol`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn run(args: OtelcolArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

pub async fn run_inner(args: OtelcolArgs) -> Result<(), String> {
    let otelcol_path = otelcol_binary_path()?;

    debug!(
        otelcol_path = %otelcol_path.display(),
        args = ?args.args,
        "Running otelcol with passthrough args"
    );

    let mut child = TokioCommand::new(otelcol_path)
        .args(&args.args)
        .spawn()
        .map_err(|e| format!("Failed to spawn otelcol: {e}"))?;

    select! {
        status = child.wait() => {
            let status = status.map_err(|e| format!("Failed to wait for otelcol: {e}"))?;

            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "otelcol exited with status {}",
                    status.code().unwrap_or(1)
                ))
            }
        }

        _ = signal::ctrl_c() => {
            debug!("Ctrl+C received, stopping otelcol");
            let _ = child.kill().await;
            Ok(())
        }
    }
}
