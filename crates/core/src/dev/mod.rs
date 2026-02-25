pub(crate) mod backend;
/// HTTP client for dev server health checks and control endpoints.
pub mod client;
/// Shared types and constants for dev server management.
pub mod common;
pub(crate) mod embedded_db;
pub(crate) mod frontend;
pub mod logging;
/// OpenTelemetry log forwarding to the flux collector.
pub mod otel;
/// Subprocess management for backend and frontend processes.
pub mod process;
/// Reverse proxy layer for API and UI requests.
pub mod proxy;
/// Axum-based dev server entry point and configuration.
pub mod server;
/// Dev token generation for inter-process authentication.
pub mod token;
pub(crate) mod watcher;
