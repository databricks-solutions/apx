//! Python ASGI boundary layer.
//!
//! Translates Rust domain types (InboundRequest, OutboundResponse) to/from
//! ASGI protocol objects (scope, receive, send).

pub mod app;
pub mod channel_body;
pub mod dispatch;
pub mod queue;
pub mod scope;
pub mod slot_receive;
pub mod slot_send;
pub mod streaming;
