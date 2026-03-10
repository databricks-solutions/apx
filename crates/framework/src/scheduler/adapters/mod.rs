//! Adapters that expose the Rust scheduler to Python frameworks.
//!
//! - [`anyio_backend`] -- AnyIO backend for Starlette/FastAPI
//! - [`cancel_scope`] -- Rust-native CancelScope for anyio
//! - [`task_group`] -- Rust-native TaskGroup for anyio
//! - [`memory_stream`] -- Rust-native MemoryObjectStream for anyio

pub mod anyio_backend;
pub mod cancel_scope;
pub mod memory_stream;
pub mod task_group;
