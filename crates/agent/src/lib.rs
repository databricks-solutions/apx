//! APX Agent - Standalone OTLP log collector
//!
//! This crate provides the `apx-agent` binary, a standalone OpenTelemetry
//! log collector that receives OTLP logs and stores them in SQLite.

pub mod server;

pub use server::run_server;
