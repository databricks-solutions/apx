//! Command-line interface for the apx toolkit.
//!
//! This crate implements the `apx` CLI, providing subcommands for project
//! initialization, building, development server management, frontend tooling,
//! and more.

pub(crate) mod __generate_openapi;
pub(crate) mod build;
pub(crate) mod bun;
pub(crate) mod common;
pub(crate) mod components;
pub(crate) mod dev;
pub(crate) mod feedback;
pub(crate) mod flux;
pub(crate) mod frontend;
pub(crate) mod info;
/// Project initialization wizard and template rendering.
pub mod init;
pub(crate) mod skill;
pub(crate) mod upgrade;

use clap::{CommandFactory, Parser, Subcommand};
use std::future::Future;

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
    /// 🧠 Skill commands (Claude Code integration)
    #[command(subcommand)]
    Skill(SkillCommands),
    /// 💬 Send feedback to the apx team
    Feedback(feedback::FeedbackArgs),
    /// ℹ️  Show environment and version info
    Info(info::InfoArgs),
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
enum SkillCommands {
    /// Install Claude Code skill files into the current project (or globally with --global)
    Install(skill::install::InstallArgs),
}

#[derive(Subcommand)]
enum FluxCommands {
    /// Start the flux OTEL collector daemon
    Start(flux::start::StartArgs),
    /// Stop the flux OTEL collector daemon
    Stop(flux::stop::StopArgs),
}

/// Standard Unix exit code for processes terminated by SIGINT (128 + signal number 2).
/// Used when the top-level Ctrl+C handler cancels the running command.
const EXIT_CODE_SIGINT: i32 = 130;

/// Parse CLI arguments and execute the corresponding subcommand.
///
/// Returns an exit code (0 for success, non-zero for failure).
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
    // Handle Ctrl+C at the top level instead of in a spawned background task.
    //
    // `tokio::signal::ctrl_c()` permanently replaces the OS default SIGINT handler,
    // so the process will NOT self-terminate on Ctrl+C — we must handle it explicitly.
    //
    // Using `select!` here (rather than a competing `tokio::spawn`) avoids a race:
    // when SIGINT arrives, the command future is dropped cooperatively at its next
    // `.await` point, and `run_cli_async` returns normally. This lets the parent
    // process (`uv run`) call `waitpid()` cleanly instead of getting ESRCH.
    //
    // Command-level Ctrl+C handlers (in `follow_logs`, `bun`, `server`, etc.) still
    // work: if their inner `select!` processes the signal first, the command future
    // completes and this outer `select!` takes the `run_command` branch instead.
    let exit_code = tokio::select! {
        code = run_command(args) => code,
        _ = tokio::signal::ctrl_c() => EXIT_CODE_SIGINT,
    };

    // Restore cursor visibility — covers both normal exit and Ctrl+C.
    // dialoguer hides the cursor during interactive widgets; this ensures
    // it reappears regardless of which select! branch won.
    let _ = console::Term::stderr().show_cursor();
    exit_code
}

async fn run_command(args: Vec<String>) -> i32 {
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
            Some(Commands::Skill(skill_cmd)) => match skill_cmd {
                SkillCommands::Install(args) => skill::install::run(args).await,
            },
            Some(Commands::Feedback(args)) => feedback::run(args).await,
            Some(Commands::Info(args)) => info::run(args).await,
            Some(Commands::Upgrade) => upgrade::run().await,
            Some(Commands::GenerateOpenapi(args)) => __generate_openapi::run(args).await,
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

/// Run an async closure and convert its `Result` into an exit code.
///
/// Returns 0 on success. On error, prints the message to stderr and returns 1.
pub async fn run_cli_async_helper<F, Fut>(f: F) -> i32
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    match f().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

#[cfg(test)]
mod test_check_flow;
