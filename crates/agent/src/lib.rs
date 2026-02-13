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

//! APX Agent - Standalone OTLP log collector
//!
//! This crate provides the `apx-agent` binary, a standalone OpenTelemetry
//! log collector that receives OTLP logs and stores them in SQLite.

pub mod server;

pub use server::run_server;
