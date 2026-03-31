//! Python ASGI boundary layer.
//!
//! Translates Rust domain types (InboundRequest, OutboundResponse) to/from
//! ASGI protocol objects (scope, receive, send).

/// ASGI protocol version string.
pub const ASGI_VERSION: &str = "3.0";

/// ASGI spec version string.
pub const ASGI_SPEC_VERSION: &str = "2.4";

pub mod app;
pub mod channel_body;
pub mod dispatch;
pub mod lifespan;
pub mod queue;
pub mod scope;
pub mod slot_receive;
pub mod slot_send;
pub mod streaming;
