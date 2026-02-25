//! MCP (Model Context Protocol) server for the apx toolkit.
//!
//! Exposes development tools (start/stop/restart dev server, type checks,
//! OpenAPI regeneration, component search, Databricks logs, SDK docs search)
//! over the MCP protocol via stdio transport.

/// Shared application context passed to every MCP handler.
pub mod context;
/// Background index initialization (component search, SDK docs).
pub mod indexing;
/// Static informational content embedded in the MCP server.
pub mod info_content;
/// MCP resource and resource-template providers.
pub mod resources;
/// MCP server setup, tool routing, and `ServerHandler` implementation.
pub mod server;
/// Tool handler implementations grouped by domain.
pub mod tools;
/// Path validation helpers for MCP tool arguments.
pub mod validation;
