//! `apx serve` — run the Python app with the apx framework runtime.
//!
//! Detects whether this process is a worker (env `APX_WORKER_NONCE` set)
//! or the supervisor, then delegates accordingly.

use std::path::PathBuf;
use std::time::Duration;

/// Validate that the manifest file exists (clap value_parser).
fn validate_manifest_path(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if path.exists() {
        Ok(path)
    } else {
        Err(format!("manifest file not found: {}", path.display()))
    }
}

/// CLI arguments for `apx serve`.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Path to pre-built manifest JSON (produced by `apx build`).
    #[arg(value_parser = validate_manifest_path)]
    manifest: PathBuf,

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
}

/// Run the serve command.
///
/// Returns 0 on success, 1 on error.
pub async fn run(args: ServeArgs) -> i32 {
    // Mode detection: APX_WORKER_NONCE present → worker, absent → supervisor.
    match apx_framework::runtime::worker::connect_to_supervisor().await {
        Ok(Some((channel, bootstrap))) => {
            // Worker mode.
            tracing::info!("running as worker");
            if let Err(e) = apx_framework::runtime::worker::run_worker(channel, bootstrap).await {
                eprintln!("Worker error: {e}");
                return 1;
            }
        }
        Ok(None) => {
            // Supervisor mode — load manifest to extract app_module.
            let manifest = match apx_framework::manifest::load(&args.manifest) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Failed to load manifest '{}': {e}", args.manifest.display());
                    return 1;
                }
            };
            let meta = match apx_framework::manifest::validate_for_serving(&manifest) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Invalid manifest: {e}");
                    return 1;
                }
            };

            let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config = apx_framework::runtime::supervisor::SupervisorConfig {
                host: args.host,
                port: args.port,
                workers: args.workers,
                app_module: meta.app_module.clone(),
                app_dir,
                request_timeout: Duration::from_secs(args.timeout),
                manifest_path: args.manifest,
            };

            if let Err(e) = apx_framework::runtime::supervisor::run_supervisor(config).await {
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
