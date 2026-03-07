//! `apx serve` — run the Python app with the apx framework runtime.
//!
//! Detects whether this process is a worker (env `APX_WORKER_NONCE` set)
//! or the supervisor, then delegates accordingly.

use apx_framework::route::AppModule;
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

    /// Python module path (e.g., "backend.app").
    #[arg(long, default_value = "backend.app")]
    app: String,

    /// Path to pre-built manifest JSON. Skips live FastAPI discovery.
    #[arg(long, value_parser = validate_manifest_path)]
    manifest: Option<PathBuf>,
}

/// Run the serve command.
///
/// Returns 0 on success, 1 on error.
pub async fn run(args: ServeArgs) -> i32 {
    let app_module = match AppModule::new(&args.app) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Invalid app module '{}': {e}", args.app);
            return 1;
        }
    };

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
            // Supervisor mode.
            let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config = apx_framework::runtime::supervisor::SupervisorConfig {
                host: args.host,
                port: args.port,
                workers: args.workers,
                app_module,
                app_dir,
                request_timeout: Duration::from_secs(args.timeout),
                manifest_path: args.manifest,
                cors: apx_framework::CorsConfig::default(),
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
