//! APX Agent — local agent powered by Databricks-hosted Foundation Models.
//!
//! This crate provides model discovery and a rig-based completions client
//! for Databricks serving endpoints.

/// Agent client for model discovery and completions.
pub mod client;
/// Error types for the agent crate.
pub mod error;
/// Model reference types and filtering utilities.
pub mod model;

pub use client::AgentClient;
pub use error::{AgentError, Result};
pub use model::{ModelRef, chat_models};
