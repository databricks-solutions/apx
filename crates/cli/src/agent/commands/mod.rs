//! Slash-command dispatch and execution for the agent TUI.

mod model;

use apx_agent::{AgentClient, CommandArgs, CommandName};

/// Outcome of executing a slash command.
pub enum CommandOutcome {
    /// User wants to quit the session.
    Quit,
    /// Model was changed to the given name.
    ModelChanged(String),
    /// Informational text to display.
    Info(&'static str),
    /// An error occurred while executing the command.
    CommandError(String),
}

/// Read-only references needed by command handlers.
pub struct CommandContext<'a> {
    pub client: &'a AgentClient,
}

/// Whether the given command requires suspending TUI raw mode before
/// execution (e.g. because it uses `dialoguer`).
pub fn needs_terminal_suspend(name: &CommandName) -> bool {
    matches!(name.as_str(), "model")
}

/// Dispatch a parsed command to the appropriate handler.
pub async fn dispatch(
    name: &CommandName,
    args: &CommandArgs,
    ctx: CommandContext<'_>,
) -> CommandOutcome {
    match name.as_str() {
        "exit" | "quit" => CommandOutcome::Quit,
        "model" => model::run(args, ctx).await,
        "help" => CommandOutcome::Info(HELP_TEXT),
        _ => CommandOutcome::CommandError(format!(
            "Unknown command: /{name}. Type /help for available commands."
        )),
    }
}

const HELP_TEXT: &str = "\
Available commands:
  /model   — Switch to a different model
  /help    — Show this help message
  /exit    — Quit the chat session";
