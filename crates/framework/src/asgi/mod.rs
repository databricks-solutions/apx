//! Python ASGI boundary layer.
//!
//! Translates Rust domain types (InboundRequest, OutboundResponse) to/from
//! ASGI protocol objects (scope, receive, send).

pub mod app;
pub mod dispatch;
pub mod scope;
pub mod streaming;
