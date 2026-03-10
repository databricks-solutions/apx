//! Adapters that expose the Rust scheduler to Python frameworks.
//!
//! - [`anyio_backend`] -- AnyIO backend for Starlette/FastAPI
//! - [`asyncio_shim`] -- asyncio monkeypatch layer

pub mod anyio_backend;
pub mod asyncio_shim;
