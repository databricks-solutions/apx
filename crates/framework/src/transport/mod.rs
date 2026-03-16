//! Transport layer abstraction.
//!
//! Separates transport-specific code (TCP/QUIC/Unix/in-memory) from the
//! application layer. The [`Listener`] trait is the binding point.

pub mod listener;
pub mod tcp;
pub mod types;

pub use listener::{Listener, TransportConfig, TransportError};
pub use tcp::TcpListener;
pub use types::{
    BodyError, BodyStream, InboundRequest, OutboundResponse, ProtocolVersion, ResponseBody,
};
