use clap::{CommandFactory, Parser, Subcommand};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod api_generator;
mod cli;
mod common;
mod dev;
pub mod dotenv;

pub use api_generator::generate_openapi;

#[cfg(target_os = "windows")]
const BUN_FILENAME: &str = "bun.exe";
#[cfg(not(target_os = "windows"))]
const BUN_FILENAME: &str = "bun";

#[derive(Parser)]
#[command(
    name = "apx",
    version,
    about = "apx is the toolkit for building Databricks Apps"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new project
    Init(cli::init::InitArgs),
    /// Build the project
    Build(cli::build::BuildArgs),
    /// Start the MCP server
    Mcp,
    /// Development server commands
    #[command(subcommand)]
    Dev(DevCommands),
    /// Internal: generate OpenAPI schema and client
    #[command(name = "__generate_openapi", hide = true)]
    GenerateOpenapi(cli::__generate_openapi::GenerateOpenapiArgs),
}

#[derive(Subcommand)]
enum DevCommands {
    /// Start development servers in detached mode
    Start(cli::dev::start::StartArgs),
    /// Check the status of development servers
    Status,
    /// Stop development servers
    Stop(cli::dev::stop::StopArgs),
    /// Restart development servers
    Restart,
    /// Display logs from development servers
    Logs(cli::dev::logs::LogsArgs),
    /// Check the project code for errors
    Check,
    /// Apply an addon to an existing project
    Apply,
    /// Internal: run dev server
    #[command(name = "__internal__run_server", hide = true)]
    InternalRunServer(cli::dev::__internal_run_server::InternalRunServerArgs),
}

#[pyfunction]
fn run_cli(args: Vec<String>) -> i32 {
    match Cli::try_parse_from(args) {
        Ok(cli) => match cli.command {
            Some(Commands::Init(init_args)) => cli::init::run(init_args),
            Some(Commands::Build(build_args)) => cli::build::run(build_args),
            Some(Commands::Mcp) => {
                println!("Starting MCP server...");
                0
            }
            Some(Commands::Dev(dev_cmd)) => match dev_cmd {
                DevCommands::Start(args) => cli::dev::start::run(args),
                DevCommands::Status => {
                    println!("Checking status...");
                    0
                }
                DevCommands::Stop(args) => cli::dev::stop::run(args),
                DevCommands::Restart => {
                    println!("Restarting development servers...");
                    0
                }
                DevCommands::Logs(args) => cli::dev::logs::run(args),
                DevCommands::Check => {
                    println!("Checking project code...");
                    0
                }
                DevCommands::Apply => {
                    println!("Applying addon...");
                    0
                }
                DevCommands::InternalRunServer(args) => cli::dev::__internal_run_server::run(args),
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

    let filter = match std::env::var("APX_LOG") {
        Ok(level) if is_plain_level(&level) => {
            format!("{crate_root}::={level}")
        }
        Ok(spec) => spec,
        Err(_) => format!("{crate_root}::=info"),
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true) // keep on while debugging
        .with_line_number(true)
        .with_file(true)
        .with_filter(EnvFilter::new(filter));

    let apx_layer = std::env::var("APX_COLLECT_LOGS").ok().map(|_| {
        dev::logging::ApxLogLayer
            .with_filter(EnvFilter::new(format!("{crate_root}::=debug")))
    });

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(apx_layer)
        .init();
}

fn is_plain_level(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    )
}

#[pyfunction]
fn get_bun_binary_path(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let bun_path = resolve_bun_binary_path(py)?;
    let pathlib = py.import("pathlib")?;
    let path_cls = pathlib.getattr("Path")?;
    let path_obj = path_cls.call1((bun_path.to_string_lossy().as_ref(),))?;
    Ok(path_obj.unbind())
}

pub(crate) fn bun_binary_path() -> Result<PathBuf, String> {
    Python::attach(|py| {
        resolve_bun_binary_path(py)
            .map_err(|err| format!("Failed to resolve bun binary path: {err}"))
    })
}

fn resolve_bun_binary_path(py: Python<'_>) -> PyResult<PathBuf> {
    let importlib = py.import("importlib.resources")?;
    let files = importlib.getattr("files")?;
    let apx_resources = files.call1(("apx",))?;
    let binaries_dir = apx_resources.getattr("joinpath")?.call1(("binaries",))?;
    let bun_path = binaries_dir.getattr("joinpath")?.call1((BUN_FILENAME,))?;
    let fspath = bun_path.getattr("__fspath__")?.call0()?;
    let bun_path_str: String = fspath.extract()?;
    Ok(PathBuf::from(bun_path_str))
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
    Ok(())
}

#[pyfunction(name = "generate_openapi")]
fn generate_openapi_py(project_root: PathBuf, force: bool) -> PyResult<bool> {
    api_generator::generate_openapi(&project_root, force)
        .map_err(|err| PyRuntimeError::new_err(err))
}
