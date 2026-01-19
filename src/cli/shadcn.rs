use clap::Args;
use tokio::process::Command as TokioCommand;
use tokio::select;
use tokio::signal;
use tracing::debug;

use crate::bun_binary_path;
use crate::cli::run_cli_async;

#[derive(Args, Debug, Clone)]
pub struct ShadcnArgs {
    /// Arguments passed directly to shadcn
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn run(args: ShadcnArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

pub async fn run_inner(args: ShadcnArgs) -> Result<(), String> {
    let bun_path = bun_binary_path()?;

    // Fixed prefix: bun x --bun shadcn@latest
    let mut full_args = vec![
        "x".to_string(),
        "--bun".to_string(),
        "shadcn@latest".to_string(),
    ];

    // Append user-provided args
    full_args.extend(args.args);

    debug!(
        bun_path = %bun_path.display(),
        args = ?full_args,
        "Running shadcn via bun"
    );

    let mut child = TokioCommand::new(bun_path)
        .args(&full_args)
        .spawn()
        .map_err(|e| format!("Failed to spawn shadcn: {e}"))?;

    select! {
        status = child.wait() => {
            let status = status.map_err(|e| format!("Failed to wait for shadcn: {e}"))?;

            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "shadcn exited with status {}",
                    status.code().unwrap_or(1)
                ))
            }
        }

        _ = signal::ctrl_c() => {
            debug!("Ctrl+C received, stopping shadcn");
            let _ = child.kill().await;
            Ok(())
        }
    }
}
