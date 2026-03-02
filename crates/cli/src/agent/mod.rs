//! Agent commands — interactive chat with Databricks-hosted models.

pub mod chat;
pub mod tui;

use clap::Subcommand;

/// Agent subcommands.
#[derive(Subcommand)]
pub enum AgentCommands {
    /// Start an interactive chat session
    Chat(chat::ChatArgs),
}
