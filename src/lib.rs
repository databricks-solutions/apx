#![forbid(unsafe_code)]
#![deny(warnings, unused_must_use, dead_code, missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

use clap::{CommandFactory, Parser, Subcommand};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod agent;
mod api_generator;
mod cli;
mod common;
mod databricks_sdk_doc;
mod dev;
pub mod dotenv;
mod flux;
mod interop;
mod mcp;
mod openapi;
mod registry;
mod search;
mod sources;

pub use api_generator::generate_openapi;
pub(crate) use interop::bun_binary_path;

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn set_app_dir(app_dir: PathBuf) -> Result<(), String> {
    if let Some(existing) = APP_DIR.get() {
        if existing != &app_dir {
            return Err(format!(
                "App directory already set to {}",
                existing.display()
            ));
        }
        return Ok(());
    }
    APP_DIR
        .set(app_dir)
        .map_err(|_| "Failed to set app directory".to_string())
}

pub(crate) fn get_app_dir() -> Option<PathBuf> {
    APP_DIR.get().cloned()
}

#[derive(Parser)]
#[command(
    name = "apx",
    version,
    about = "\x1b[33mapx\x1b[0m is the toolkit for building Databricks Apps 🚀"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 🎬 Initialize a new project
    Init(cli::init::InitArgs),
    /// 🔨 Build the project
    Build(cli::build::BuildArgs),
    /// 🍞 Run a command using bun
    Bun(cli::bun::BunArgs),
    /// 🧩 Components commands
    #[command(subcommand)]
    Components(ComponentsCommands),
    /// 🎨 Frontend commands
    #[command(subcommand)]
    Frontend(FrontendCommands),
    /// 🔌 Start the MCP server
    Mcp,
    /// 🚀 Development server commands
    #[command(subcommand)]
    Dev(DevCommands),
    /// 📊 Flux OTEL collector commands
    #[command(subcommand)]
    Flux(FluxCommands),
    /// Internal: generate OpenAPI schema and client
    #[command(name = "__generate_openapi", hide = true)]
    GenerateOpenapi(cli::__generate_openapi::GenerateOpenapiArgs),
}

#[derive(Subcommand)]
enum ComponentsCommands {
    /// Run a shadcn command
    Add(cli::components::add::ComponentsAddArgs),
}

#[derive(Subcommand)]
enum FrontendCommands {
    /// Run the frontend development server
    Dev(cli::frontend::dev::DevArgs),
    /// Build the frontend
    Build(cli::frontend::build::BuildArgs),
}

#[derive(Subcommand)]
enum DevCommands {
    /// Start development servers in detached mode
    Start(cli::dev::start::StartArgs),
    /// Check the status of development servers
    Status(cli::dev::status::StatusArgs),
    /// Stop development servers
    Stop(cli::dev::stop::StopArgs),
    /// Restart development servers
    Restart(cli::dev::restart::RestartArgs),
    /// Display logs from development servers
    Logs(cli::dev::logs::LogsArgs),
    /// Check the project code for errors
    Check(cli::dev::check::CheckArgs),
    /// Apply an addon to an existing project
    Apply,
    /// Internal: run dev server
    #[command(name = "__internal__run_server", hide = true)]
    InternalRunServer(cli::dev::__internal_run_server::InternalRunServerArgs),
}

#[derive(Subcommand)]
enum FluxCommands {
    /// Start the flux OTEL collector daemon
    Start(cli::flux::start::StartArgs),
    /// Stop the flux OTEL collector daemon
    Stop(cli::flux::stop::StopArgs),
}

#[pyfunction]
fn run_cli(args: Vec<String>) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("Failed to create tokio runtime: {err}");
            return 1;
        }
    };

    runtime.block_on(run_cli_async(args))
}

async fn run_cli_async(args: Vec<String>) -> i32 {
    match Cli::try_parse_from(args) {
        Ok(cli) => match cli.command {
            Some(Commands::Init(init_args)) => cli::init::run(init_args).await,
            Some(Commands::Build(build_args)) => cli::build::run(build_args).await,
            Some(Commands::Bun(bun_args)) => cli::bun::run(bun_args).await,
            Some(Commands::Components(components_cmd)) => match components_cmd {
                ComponentsCommands::Add(args) => cli::components::add::run(args).await,
            },
            Some(Commands::Frontend(frontend_cmd)) => match frontend_cmd {
                FrontendCommands::Dev(args) => cli::frontend::dev::run(args).await,
                FrontendCommands::Build(args) => cli::frontend::build::run(args).await,
            },
            Some(Commands::Mcp) => cli::dev::mcp::run(cli::dev::mcp::McpArgs {}).await,
            Some(Commands::Dev(dev_cmd)) => match dev_cmd {
                DevCommands::Start(args) => cli::dev::start::run(args).await,
                DevCommands::Status(args) => cli::dev::status::run(args).await,
                DevCommands::Stop(args) => cli::dev::stop::run(args).await,
                DevCommands::Restart(args) => cli::dev::restart::run(args).await,
                DevCommands::Logs(args) => cli::dev::logs::run(args).await,
                DevCommands::Check(args) => cli::dev::check::run(args).await,
                DevCommands::Apply => {
                    println!("Applying addon...");
                    0
                }
                DevCommands::InternalRunServer(args) => {
                    cli::dev::__internal_run_server::run(args).await
                }
            },
            Some(Commands::Flux(flux_cmd)) => match flux_cmd {
                FluxCommands::Start(args) => cli::flux::start::run(args).await,
                FluxCommands::Stop(args) => cli::flux::stop::run(args).await,
            },
            Some(Commands::GenerateOpenapi(args)) => cli::__generate_openapi::run(args),
            None => {
                let mut cmd = Cli::command();
                let _ = cmd.print_help();
                println!();
                0
            }
        },
        Err(e) => {
            let code = e.exit_code();
            let _ = e.print();
            code
        }
    }
}

pub(crate) fn init_tracing() {
    let crate_root = module_path!().to_string();

    // APX_LOG controls log level: "trace", "debug", "info", "warn", "error"
    // or a full tracing filter spec like "apx=debug,tower_http=warn"
    let filter = match std::env::var("APX_LOG") {
        Ok(level) if is_plain_level(&level) => {
            format!("{crate_root}={level}")
        }
        Ok(spec) => spec,
        Err(_) => format!("{crate_root}=info"),
    };

    // Check if OTLP logging is enabled (set by dev server subprocess)
    let otel_enabled = std::env::var("APX_OTEL_LOGS").is_ok_and(|v| v == "1");

    // Get app directory from environment (set by start.rs when spawning dev server)
    let app_dir = std::env::var("APX_APP_DIR").ok();

    if otel_enabled {
        // Initialize with both fmt and OTLP layers
        if let Err(e) = init_tracing_with_otel(&crate_root, &filter, app_dir.as_deref()) {
            eprintln!("Warning: Failed to initialize OTLP logging: {e}");
            init_tracing_fmt_only(&filter);
        }
    } else {
        init_tracing_fmt_only(&filter);
    }
}

fn init_tracing_fmt_only(filter: &str) {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_line_number(true)
        .with_file(true)
        .with_filter(EnvFilter::new(filter));

    if tracing_subscriber::registry()
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Warning: tracing subscriber already initialized");
    }
}

fn init_tracing_with_otel(
    service_name: &str,
    filter: &str,
    app_dir: Option<&str>,
) -> Result<(), String> {
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::logs::SdkLoggerProvider;

    let endpoint = format!("http://127.0.0.1:{}/v1/logs", flux::FLUX_PORT);

    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| format!("Failed to create OTLP exporter: {e}"))?;

    // Build resource attributes including app_path if available
    let mut attributes = vec![KeyValue::new("service.name", service_name.to_string())];
    if let Some(app_path) = app_dir {
        attributes.push(KeyValue::new("apx.app_path", app_path.to_string()));
    }

    let provider = SdkLoggerProvider::builder()
        .with_resource(Resource::builder().with_attributes(attributes).build())
        .with_batch_exporter(exporter)
        .build();

    let otel_layer =
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider)
            .with_filter(EnvFilter::new(filter));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_line_number(true)
        .with_file(true)
        .with_filter(EnvFilter::new(filter));

    if tracing_subscriber::registry()
        .with(otel_layer)
        .with(fmt_layer)
        .try_init()
        .is_err()
    {
        eprintln!("Warning: tracing subscriber already initialized");
    }

    Ok(())
}

fn is_plain_level(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    )
}

#[pyfunction]
fn get_bun_binary_path(py: Python<'_>) -> PyResult<Py<PyAny>> {
    interop::get_bun_binary_path(py)
}

#[pyfunction]
fn get_dotenv_vars() -> PyResult<HashMap<String, String>> {
    use tracing::warn;

    let app_dir = get_app_dir()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| PyRuntimeError::new_err("Failed to determine app directory"))?;

    let dotenv_path = app_dir.join(".env");

    if !dotenv_path.exists() {
        warn!(
            ".env file not found at {}, using empty environment",
            dotenv_path.display()
        );
        return Ok(HashMap::new());
    }

    let dotenv = dotenv::DotenvFile::read(&dotenv_path).map_err(PyRuntimeError::new_err)?;
    Ok(dotenv.get_vars())
}

/// A Python module implemented in Rust. The name of this module must match
/// the `lib.name` setting in the `Cargo.toml`, else Python will not be able to
/// import the module.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    init_tracing();
    m.add_function(wrap_pyfunction!(run_cli, m)?)?;
    m.add_function(wrap_pyfunction!(get_bun_binary_path, m)?)?;
    m.add_function(wrap_pyfunction!(generate_openapi_py, m)?)?;
    m.add_function(wrap_pyfunction!(get_dotenv_vars, m)?)?;
    Ok(())
}

#[pyfunction(name = "generate_openapi")]
fn generate_openapi_py(project_root: PathBuf) -> PyResult<()> {
    api_generator::generate_openapi(&project_root).map_err(PyRuntimeError::new_err)
}
