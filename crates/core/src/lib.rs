#![forbid(unsafe_code)]
#![deny(warnings, unused_must_use, dead_code, missing_debug_implementations)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout
)]

pub mod agent;
pub mod api_generator;
pub mod app_state;
pub mod common;
pub mod components;
pub mod databricks_sdk_doc;
pub mod dev;
pub mod dotenv;
pub mod download;
pub mod flux;
pub mod frontend;
pub mod interop;
pub mod ops;
pub mod py_edit;
pub mod resources;
pub mod search;
pub mod tracing_init;

pub(crate) mod openapi;
pub(crate) mod python_logging;
pub(crate) mod registry;
pub(crate) mod sources;

pub use download::{BinarySource, ResolvedBinary, resolve_bun, resolve_uv};
pub use openapi::generate as generate_openapi_ts;
