//! Core library for the apx application framework.
//!
//! Contains business logic for project scaffolding, dev server management,
//! component registries, SDK documentation search, and Databricks integration.

#![deny(clippy::print_stdout)]

/// Agent integration utilities.
pub mod agent;
/// OpenAPI spec generation and TypeScript client codegen.
pub mod api_generator;
/// Global application directory state.
pub mod app_state;
/// Common types, project metadata, and CLI utilities.
pub mod common;
/// UI component registry operations (search, add, CSS updates).
pub mod components;
/// Databricks SDK documentation parsing and indexing.
pub mod databricks_sdk_doc;
/// Dev server lifecycle (process management, proxy, logging).
pub mod dev;
/// `.env` file reader and writer.
pub mod dotenv;
/// Binary download helpers for managed tool installs.
pub mod download;
/// External tool abstraction (uv, bun, git, gh, databricks CLI).
pub mod external;
/// User feedback issue creation (GitHub).
pub mod feedback;
/// Flux log collector integration.
pub mod flux;
/// Frontend build and scaffolding utilities.
pub mod frontend;
/// Python interop (OpenAPI generation, SDK version detection).
pub mod interop;
/// High-level operations (dev server, check, logs, healthcheck).
pub mod ops;
/// Python source-code editing (AST-based import insertion).
pub mod py_edit;
/// Embedded template resources.
pub mod resources;
/// Full-text search indexes (component search, SDK docs).
pub mod search;
/// Tracing / logging initialization.
pub mod tracing_init;

/// OpenAPI specification parsing, TypeScript codegen, and related utilities.
pub mod openapi;
pub(crate) mod python_logging;
pub(crate) mod registry;
pub(crate) mod sources;

pub use external::{BinarySource, ResolvedBinary};
pub use openapi::generate as generate_openapi_ts;
