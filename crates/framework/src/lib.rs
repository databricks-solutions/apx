//! Python framework embedding via PyO3 for apx.
//!
//! This crate implements the apx framework runtime: a Rust-powered HTTP server
//! that serves Python handlers directly via PyO3, replacing both uvicorn and
//! FastAPI for the serving path.
//!
//! # Architecture
//!
//! - **Supervisor** spawns N worker processes, each with its own Python interpreter
//! - **Workers** bind `SO_REUSEPORT` TCP listeners — the kernel distributes connections
//! - **IPC** between supervisor and workers uses length-prefixed msgpack over UDS
//! - **Bridge** calls Python async handlers from axum via PyO3 + `pyo3-async-runtimes`

pub mod error;
pub mod manifest;
pub mod pyapi;
pub mod route;
pub mod runtime;
pub mod transport;

pub(crate) mod bridge;
pub(crate) mod discovery;
pub(crate) mod event_loop;
pub(crate) mod ipc;
pub(crate) mod scheduler;
pub(crate) mod signal;

#[cfg(test)]
pub(crate) fn with_py<R>(f: impl FnOnce(pyo3::Python<'_>) -> R) -> R {
    integration_tests::ensure_python_env();
    pyo3::Python::initialize();
    pyo3::Python::attach(f)
}

#[cfg(test)]
mod integration_tests;
