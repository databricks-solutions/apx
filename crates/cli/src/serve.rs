//! `apx serve` — run the Python app with the apx framework runtime.
//!
//! Detects whether this process is a worker (env `APX_WORKER_NONCE` set)
//! or the supervisor, then delegates accordingly.

use std::path::PathBuf;
use std::time::Duration;

/// CLI arguments for `apx serve`.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// App module (e.g. "backend.app") or path to manifest JSON.
    ///
    /// If the target is a file that exists on disk, it is treated as a
    /// pre-built manifest (produced by `apx build`). Otherwise it is
    /// interpreted as a Python module path and the app is imported live
    /// at worker startup.
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
}

/// Resolve the CLI target into an `(app_module, manifest_path)` pair.
///
/// If the target is an existing file, it's a manifest path — we load it
/// to extract the app_module. Otherwise it's a Python module string and
/// we run in live-import mode (no manifest file).
fn resolve_target(
    target: &str,
) -> Result<(apx_framework::route::AppModule, Option<PathBuf>), String> {
    let path = PathBuf::from(target);
    if path.exists() {
        // Manifest-based path.
        let manifest = apx_framework::manifest::load(&path)
            .map_err(|e| format!("failed to load manifest '{}': {e}", path.display()))?;
        let meta = apx_framework::manifest::validate_for_serving(&manifest)
            .map_err(|e| format!("invalid manifest: {e}"))?;
        Ok((meta.app_module.clone(), Some(path)))
    } else {
        // Live-import path — target is a Python module string.
        let app_module = apx_framework::route::AppModule::new(target)
            .map_err(|e| format!("invalid app module '{target}': {e}"))?;
        Ok((app_module, None))
    }
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
            // Supervisor mode — resolve target.
            let (app_module, manifest_path) = match resolve_target(&args.target) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            };

            let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let config = apx_framework::runtime::supervisor::SupervisorConfig {
                host: args.host,
                port: args.port,
                workers: args.workers,
                app_module,
                app_dir,
                request_timeout: Duration::from_secs(args.timeout),
                manifest_path,
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
