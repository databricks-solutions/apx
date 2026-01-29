use clap::Args;
use std::net::TcpListener;
use std::path::PathBuf;
use tracing::{debug, warn};

use crate::cli::run_cli_async;
use crate::dev::common::{
    BACKEND_PORT_END, BACKEND_PORT_START, BIND_HOST, DB_PORT_END, DB_PORT_START, FRONTEND_PORT_END,
    FRONTEND_PORT_START, find_random_port_in_range,
};
use crate::dev::server::run_server;
use crate::interop::validate_credentials;
use crate::set_app_dir;

/// Maximum number of retries for subprocess port allocation
const MAX_PORT_RETRIES: u32 = 5;

#[derive(Args, Debug, Clone)]
pub struct InternalRunServerArgs {
    #[arg(long = "app-dir", value_name = "APP_PATH")]
    pub app_dir: PathBuf,
    #[arg(long = "host", default_value = BIND_HOST)]
    pub host: String,
    #[arg(long = "port")]
    pub port: u16,
    #[arg(long = "skip-credentials-validation")]
    pub skip_credentials_validation: bool,
}

pub async fn run(args: InternalRunServerArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(args: InternalRunServerArgs) -> Result<(), String> {
    set_app_dir(args.app_dir.clone())?;

    // Validate credentials before starting server (warn if skipped or failed)
    if args.skip_credentials_validation {
        warn!("Credentials validation skipped. API proxy may not work correctly.");
    } else if let Err(err) = validate_credentials() {
        warn!("Credentials validation failed: {err}. API proxy may not work correctly.");
    }

    // Try to start the server with randomized subprocess ports
    // Retry with new ports if startup fails (likely due to port conflict)
    let mut last_error = String::new();
    for attempt in 1..=MAX_PORT_RETRIES {
        // Bind the main server listener at each attempt
        // The port was already validated/reserved by the parent process (start.rs)
        let std_listener = TcpListener::bind((&*args.host, args.port))
            .map_err(|e| format!("Failed to bind main server port {}: {e}", args.port))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set listener to non-blocking: {e}"))?;
        let listener = tokio::net::TcpListener::from_std(std_listener)
            .map_err(|e| format!("Failed to convert to tokio listener: {e}"))?;

        // Use randomized port selection for subprocess ports to reduce collision probability
        // when multiple dev servers start simultaneously
        let backend_port =
            find_random_port_in_range(&args.host, BACKEND_PORT_START, BACKEND_PORT_END)?;
        let frontend_port =
            find_random_port_in_range(&args.host, FRONTEND_PORT_START, FRONTEND_PORT_END)?;
        let db_port = find_random_port_in_range(&args.host, DB_PORT_START, DB_PORT_END)?;

        debug!(
            attempt,
            backend_port, frontend_port, db_port, "Attempting to start dev server with ports"
        );

        match run_server(
            args.app_dir.clone(),
            listener,
            backend_port,
            frontend_port,
            db_port,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Check if this is a port-related error that might benefit from retry
                let is_port_error = e.contains("address already in use")
                    || e.contains("EADDRINUSE")
                    || e.contains("not ready on");

                if is_port_error && attempt < MAX_PORT_RETRIES {
                    warn!(
                        attempt,
                        error = %e,
                        "Subprocess port conflict, retrying with new ports"
                    );
                    last_error = e;
                    continue;
                }

                return Err(e);
            }
        }
    }

    Err(format!(
        "Failed to start dev server after {MAX_PORT_RETRIES} attempts. Last error: {last_error}"
    ))
}
