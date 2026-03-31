//! `apx serve` — run the Python app with the apx framework runtime.
//!
//! Detects whether this process is a worker (env `APX_WORKER_NONCE` set)
//! or the supervisor, then delegates accordingly.

use std::path::PathBuf;
use std::time::Duration;

/// CLI arguments for `apx serve`.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// App module (e.g. "backend.app").
    #[arg(value_name = "TARGET")]
    target: String,

    /// Host to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// Port to bind to.
    #[arg(long, default_value_t = 8000)]
    port: u16,

    /// Number of worker processes.
    #[arg(long, default_value_t = 1)]
    workers: usize,

    /// Request timeout in seconds (0 = no timeout).
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Maximum concurrent requests per worker (0 = default 256).
    #[arg(long, default_value_t = 0)]
    max_concurrent: usize,

    /// Event loop policy: "asyncio" (stdlib) or "uvloop" (default).
    #[arg(long = "loop", default_value = "uvloop")]
    loop_policy: String,

    /// Enable dev-mode file watcher (restarts workers on .py changes).
    #[arg(long, hide = true)]
    dev: bool,

    /// Maximum seconds to wait for workers to drain in-flight requests
    /// before warning and killing them.
    #[arg(long, default_value_t = 5)]
    drain_timeout: u64,
}

/// Validate the CLI target as a Python dotted module path.
fn resolve_target(
    target: &str,
) -> Result<apx_framework::supervision::ipc::protocol::AppModule, String> {
    apx_framework::supervision::ipc::protocol::AppModule::new(target)
        .map_err(|e| format!("invalid app module '{target}': {e}"))
}

/// Run the serve command.
///
/// Returns 0 on success, 1 on error.
pub async fn run(args: ServeArgs) -> i32 {
    // Mode detection: APX_WORKER_NONCE present → worker, absent → supervisor.
    match apx_framework::supervision::worker::connect_to_supervisor().await {
        Ok(Some((channel, bootstrap))) => {
            // Worker mode.
            tracing::debug!("running as worker");
            if let Err(e) = apx_framework::supervision::worker::run_worker(channel, bootstrap).await
            {
                eprintln!(
                    "{}",
                    apx_framework::supervision::worker::format_worker_error(&e)
                );
                return 1;
            }
        }
        Ok(None) => {
            // Supervisor mode — resolve target.
            let app_module = match resolve_target(&args.target) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            };

            let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config = apx_framework::supervision::supervisor::SupervisorConfig {
                host: args.host,
                port: args.port,
                workers: args.workers,
                app_module,
                app_dir,
                request_timeout: Duration::from_secs(args.timeout),
                max_concurrent: if args.max_concurrent == 0 {
                    None
                } else {
                    Some(args.max_concurrent)
                },
                loop_policy: args.loop_policy,
                dev_mode: args.dev,
                drain_timeout: Duration::from_secs(args.drain_timeout),
            };

            if let Err(e) = apx_framework::supervision::supervisor::run_supervisor(config).await {
                eprintln!("Supervisor error: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("Bootstrap error: {e}");
            return 1;
        }
    }

    0
}
