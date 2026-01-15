use clap::{CommandFactory, Parser, Subcommand};
use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;
use std::path::PathBuf;

mod api_generator;
mod cli;

pub use api_generator::generate_openapi;

#[cfg(target_os = "windows")]
const BUN_FILENAME: &str = "bun.exe";
#[cfg(not(target_os = "windows"))]
const BUN_FILENAME: &str = "bun";

#[derive(Parser)]
#[command(name = "apx", version, about = "apx is the toolkit for building Databricks Apps")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new project
    Init(cli::init::InitArgs),
    /// Build the project
    Build,
    /// Start the MCP server
    Mcp,
    /// Development server commands
    #[command(subcommand)]
    Dev(DevCommands),
}

#[derive(Subcommand)]
enum DevCommands {
    /// Start development servers in detached mode
    Start,
    /// Check the status of development servers
    Status,
    /// Stop development servers
    Stop,
    /// Restart development servers
    Restart,
    /// Display logs from development servers
    Logs,
    /// Check the project code for errors
    Check,
    /// Apply an addon to an existing project
    Apply,
}

#[pyfunction]
fn run_cli(args: Vec<String>) -> i32 {
    match Cli::try_parse_from(args) {
        Ok(cli) => match cli.command {
            Some(Commands::Init(init_args)) => cli::init::run(init_args),
            Some(Commands::Build) => {
                println!("Building project...");
                0
            }
            Some(Commands::Mcp) => {
                println!("Starting MCP server...");
                0
            }
            Some(Commands::Dev(dev_cmd)) => match dev_cmd {
                DevCommands::Start => {
                    println!("Starting development servers...");
                    0
                }
                DevCommands::Status => {
                    println!("Checking status...");
                    0
                }
                DevCommands::Stop => {
                    println!("Stopping development servers...");
                    0
                }
                DevCommands::Restart => {
                    println!("Restarting development servers...");
                    0
                }
                DevCommands::Logs => {
                    println!("Displaying logs...");
                    0
                }
                DevCommands::Check => {
                    println!("Checking project code...");
                    0
                }
                DevCommands::Apply => {
                    println!("Applying addon...");
                    0
                }
            },
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