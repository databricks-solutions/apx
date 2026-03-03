//! APX Agent — local agent powered by Databricks-hosted Foundation Models.
//!
//! This crate provides model discovery, streaming chat, and session persistence
//! for Databricks serving endpoints.

/// Chat domain types — roles, messages, and streaming events.
pub mod chat;
/// Agent client for model discovery and completions.
pub mod client;
/// Slash-command parsing for TUI input.
pub mod command;
/// Error types for the agent crate.
pub mod error;
/// Model reference types and filtering utilities.
pub mod model;
/// Session store trait and domain types.
pub mod session;
/// SQLite-backed session store.
pub mod session_sqlite;

pub use chat::{ChatEvent, ChatMessage, Role, now_secs};
pub use client::AgentClient;
pub use command::{CommandArgs, CommandName, ParsedInput, parse_input};
pub use error::{AgentError, Result};
pub use model::{ModelRef, chat_models};
pub use session::{Session, SessionStore};
pub use session_sqlite::SqliteSessionStore;
