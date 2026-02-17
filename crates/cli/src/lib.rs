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

pub(crate) mod __generate_openapi;
pub(crate) mod build;
pub(crate) mod bun;
pub(crate) mod common;
pub(crate) mod components;
pub(crate) mod dev;
pub(crate) mod flux;
pub(crate) mod frontend;
pub(crate) mod init;
pub(crate) mod upgrade;

use clap::{CommandFactory, Parser, Subcommand};

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
    Init(init::InitArgs),
    /// 🔨 Build the project
    Build(build::BuildArgs),
    /// 🍞 Run a command using bun
    Bun(bun::BunArgs),
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
    /// ⬆️  Upgrade apx to the latest version
    Upgrade,
    /// Internal: generate OpenAPI schema and client
    #[command(name = "__generate_openapi", hide = true)]
    GenerateOpenapi(__generate_openapi::GenerateOpenapiArgs),
}

#[derive(Subcommand)]
enum ComponentsCommands {
    /// Run a shadcn command
    Add(components::add::ComponentsAddArgs),
}

#[derive(Subcommand)]
enum FrontendCommands {
    /// Run the frontend development server
    Dev(frontend::dev::DevArgs),
    /// Build the frontend
    Build(frontend::build::BuildArgs),
}

#[derive(Subcommand)]
enum DevCommands {
    /// Start development servers in detached mode
    Start(dev::start::StartArgs),
    /// Check the status of development servers
    Status(dev::status::StatusArgs),
    /// Stop development servers
    Stop(dev::stop::StopArgs),
    /// Restart development servers
    Restart(dev::restart::RestartArgs),
    /// Display logs from development servers
    Logs(dev::logs::LogsArgs),
    /// Check the project code for errors
    Check(dev::check::CheckArgs),
    /// Apply an addon to an existing project
    Apply(dev::apply::ApplyArgs),
    /// Internal: run dev server
    #[command(name = "__internal__run_server", hide = true)]
    InternalRunServer(dev::__internal_run_server::InternalRunServerArgs),
}

#[derive(Subcommand)]
enum FluxCommands {
    /// Start the flux OTEL collector daemon
    Start(flux::start::StartArgs),
    /// Stop the flux OTEL collector daemon
    Stop(flux::stop::StopArgs),
}

pub fn run_cli(args: Vec<String>) -> i32 {
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
            Some(Commands::Init(init_args)) => init::run(init_args).await,
            Some(Commands::Build(build_args)) => build::run(build_args).await,
            Some(Commands::Bun(bun_args)) => bun::run(bun_args).await,
            Some(Commands::Components(components_cmd)) => match components_cmd {
                ComponentsCommands::Add(args) => components::add::run(args).await,
            },
            Some(Commands::Frontend(frontend_cmd)) => match frontend_cmd {
                FrontendCommands::Dev(args) => frontend::dev::run(args).await,
                FrontendCommands::Build(args) => frontend::build::run(args).await,
            },
            Some(Commands::Mcp) => dev::mcp::run(dev::mcp::McpArgs {}).await,
            Some(Commands::Dev(dev_cmd)) => match dev_cmd {
                DevCommands::Start(args) => dev::start::run(args).await,
                DevCommands::Status(args) => dev::status::run(args).await,
                DevCommands::Stop(args) => dev::stop::run(args).await,
                DevCommands::Restart(args) => dev::restart::run(args).await,
                DevCommands::Logs(args) => dev::logs::run(args).await,
                DevCommands::Check(args) => dev::check::run(args).await,
                DevCommands::Apply(args) => dev::apply::run(args).await,
                DevCommands::InternalRunServer(args) => dev::__internal_run_server::run(args).await,
            },
            Some(Commands::Flux(flux_cmd)) => match flux_cmd {
                FluxCommands::Start(args) => flux::start::run(args).await,
                FluxCommands::Stop(args) => flux::stop::run(args).await,
            },
            Some(Commands::Upgrade) => upgrade::run().await,
            Some(Commands::GenerateOpenapi(args)) => __generate_openapi::run(args),
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

pub async fn run_cli_async_helper<F, Fut>(f: F) -> i32
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    match f().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
