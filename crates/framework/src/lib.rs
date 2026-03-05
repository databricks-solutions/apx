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
pub mod pyapi;
pub mod route;
pub mod runtime;
pub mod transport;

pub(crate) mod bridge;
pub(crate) mod discovery;
pub(crate) mod ipc;
pub(crate) mod signal;
