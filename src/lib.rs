use clap::{CommandFactory, Parser, Subcommand};
use pyo3::prelude::*;

#[derive(Parser)]
#[command(name = "apx", version, about = "apx is the toolkit for building Databricks Apps")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new project
    Init,
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
            Some(Commands::Init) => {
                println!("Initializing project...");
                0
            }
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

/// A Python module implemented in Rust. The name of this module must match
/// the `lib.name` setting in the `Cargo.toml`, else Python will not be able to
/// import the module.
#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_cli, m)?)?;
    Ok(())
}