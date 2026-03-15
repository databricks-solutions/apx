//! Python framework embedding via PyO3 for apx.
//!
//! This crate implements the apx framework runtime: a Rust-powered HTTP server
//! that serves Python ASGI applications directly via PyO3.
//!
//! # Architecture
//!
//! - **Supervisor** spawns N worker processes, each with its own Python interpreter
//! - **Workers** bind `SO_REUSEPORT` TCP listeners — the kernel distributes connections
//! - **IPC** between supervisor and workers uses length-prefixed msgpack over UDS
//! - **Bridge** calls Python ASGI handlers via PyO3 + `pyo3-async-runtimes`

pub mod error;
pub mod pyapi;
pub mod runtime;
pub mod transport;

pub mod telemetry;

pub(crate) mod app_loader;
pub(crate) mod bridge;
pub(crate) mod dispatch;
pub(crate) mod driver_pool;
pub(crate) mod event_loop;
pub mod ipc;
pub(crate) mod scheduler;
pub(crate) mod service;
pub(crate) mod signal;
pub(crate) mod websocket;
pub(crate) mod worker_context;

#[cfg(test)]
pub(crate) fn with_py<R>(f: impl FnOnce(pyo3::Python<'_>) -> R) -> R {
    integration_tests::ensure_python_env();
    pyo3::Python::initialize();
    pyo3::Python::attach(f)
}

#[cfg(test)]
mod integration_tests;
