//! Python framework embedding via PyO3 for apx.
//!
//! This crate implements the apx framework: a Rust-powered HTTP server
//! that serves Python ASGI applications directly via PyO3.
//!
//! # Architecture
//!
//! - **Supervisor** spawns N worker processes, each with its own Python interpreter
//! - **Workers** bind `SO_REUSEPORT` TCP listeners — the kernel distributes connections
//! - **IPC** between supervisor and workers uses length-prefixed msgpack over UDS
//! - **ASGI** calls Python handlers via PyO3, driven by the Rust coroutine scheduler

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Implement `Debug` for types whose fields are not printable
/// (e.g. `Py<T>`, OTel metric handles, file descriptors).
/// Prints `TypeName { .. }`.
macro_rules! opaque_debug {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl ::std::fmt::Debug for $ty {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    f.debug_struct(stringify!($ty)).finish_non_exhaustive()
                }
            }
        )+
    };
}
pub(crate) use opaque_debug;

pub mod dispatch;
pub(crate) mod protocol;
pub mod pyapi;
pub mod telemetry;
pub mod transport;

pub(crate) mod asgi;
pub(crate) mod io;
pub mod supervision;

#[cfg(test)]
pub(crate) fn with_py<R>(f: impl FnOnce(pyo3::Python<'_>) -> R) -> R {
    integration_tests::ensure_python_env();
    pyo3::Python::initialize();
    pyo3::Python::attach(f)
}

#[cfg(test)]
mod integration_tests;
