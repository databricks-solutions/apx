//! Transport type definitions.

use std::net::SocketAddr;

/// HTTP protocol version.
///
/// Tracked per-request so ASGI scope can set `http_version` correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    /// HTTP/1.0.
    Http10,
    /// HTTP/1.1.
    Http11,
    /// HTTP/2.
    H2,
}

/// Errors during transport setup.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Bind to the requested address failed.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// Address we tried to bind.
        addr: SocketAddr,
        /// OS error.
        source: std::io::Error,
    },
    /// Invalid host string (not a valid IP address).
    #[error("invalid host {host:?}: {source}")]
    InvalidHost {
        /// The unparseable host string.
        host: String,
        /// Parse error.
        source: std::net::AddrParseError,
    },
}
