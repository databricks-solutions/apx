//! Python ASGI boundary layer.
//!
//! Translates Rust domain types to/from ASGI protocol objects
//! (scope, receive, send).

/// ASGI protocol version string.
pub const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
pub const ASGI_SPEC_VERSION: &str = "2.4";

pub mod app;
pub mod lifespan;
pub mod scope;
