//! APX Agent - Standalone OTLP log collector binary
//!
//! This binary runs as a daemon process, receiving OpenTelemetry logs
//! via HTTP and storing them in a local SQLite database.

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "apx-agent", version, about = "APX OTLP log collector agent")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Run the OTLP collector server (default)
    Run,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "apx_agent=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let _args = Args::parse();

    // Run server (default behavior regardless of subcommand)
    if let Err(e) = apx_agent::run_server().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
