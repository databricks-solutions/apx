//! Transport layer abstraction.
//!
//! Separates the transport-specific code (TCP/QUIC/Unix/in-memory) from the
//! application layer. The [`Listener`] trait is the binding point — each
//! transport implements it.
//!
//! # Architecture
//!
//! ```text
//! Transport (TCP today, QUIC later)
//!   → Protocol (hyper for h1/h2, quinn for h3)
//!     → InboundRequest (neutral pivot)
//!       → Application (routing → dispatch → ASGI adapter → Python)
//! ```

// TODO(phase-2): remove allow once convert module is populated
#[allow(clippy::missing_docs_in_private_items)]
mod convert;
pub mod tcp;
pub mod types;

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use types::TransportKind;

// Re-exports for convenience.
pub use tcp::TcpListener;
pub use types::{
    BodyError, BodyStream, InboundRequest, OutboundResponse, ProtocolVersion, ResponseBody,
};

/// Errors during transport operations.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The host string could not be parsed as an IP address.
    #[error("invalid host address '{host}': {source}")]
    InvalidHost {
        /// The host string that failed to parse.
        host: String,
        /// The underlying parse error.
        source: std::net::AddrParseError,
    },

    /// Socket creation failed.
    #[error("failed to create socket: {0}")]
    SocketCreate(std::io::Error),

    /// Binding to the address failed.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The socket address that failed to bind.
        addr: SocketAddr,
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// Transitioning to listen mode failed.
    #[error("failed to listen: {0}")]
    Listen(std::io::Error),

    /// Converting to a tokio listener failed.
    #[error("failed to convert to tokio listener: {0}")]
    TokioConvert(std::io::Error),

    /// Serving requests failed.
    #[error("serve failed: {0}")]
    Serve(std::io::Error),
}

/// Default TCP listen backlog.
const DEFAULT_BACKLOG: u32 = 1024;

/// Configuration for transport binding.
#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    /// IP address to bind.
    pub host: IpAddr,
    /// Port to bind.
    pub port: u16,
    /// Which transport to use.
    pub transport_kind: TransportKind,
    /// TCP listen backlog.
    pub backlog: u32,
}

impl TransportConfig {
    /// Create a TCP transport config.
    pub fn tcp(host: IpAddr, port: u16) -> Self {
        Self {
            host,
            port,
            transport_kind: TransportKind::Tcp,
            backlog: DEFAULT_BACKLOG,
        }
    }
}

/// Transport-agnostic listener trait.
///
/// v1: `TcpListener` (hyper for HTTP/1 + HTTP/2).
/// Future: `QuicListener` (quinn for HTTP/3), `UnixListener`, `InMemoryListener`.
///
/// This is an open abstraction (trait, not enum) because new transport
/// implementations may come from external crates.
pub trait Listener: Send + Sync + 'static {
    /// Bind to the configured address.
    fn bind(config: &TransportConfig) -> impl Future<Output = Result<Self, TransportError>> + Send
    where
        Self: Sized;

    /// Serve requests using the provided axum router.
    ///
    /// Runs until the shutdown signal fires.
    fn serve(
        self,
        router: axum::Router,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Return the locally bound socket address.
    fn local_addr(&self) -> SocketAddr;

    /// Return the transport kind.
    fn transport_kind(&self) -> TransportKind;
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn transport_config_tcp() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 8080);
        assert_eq!(config.host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(config.port, 8080);
        assert_eq!(config.backlog, DEFAULT_BACKLOG);
        assert!(matches!(config.transport_kind, TransportKind::Tcp));
    }

    #[test]
    fn transport_error_display_invalid_host() {
        let source = "bad".parse::<IpAddr>().unwrap_err();
        let err = TransportError::InvalidHost {
            host: "bad".to_owned(),
            source,
        };
        let msg = format!("{err}");
        assert!(msg.contains("bad"));
        assert!(msg.contains("invalid"));
    }

    #[test]
    fn transport_error_display_socket_create() {
        let err = TransportError::SocketCreate(std::io::Error::other("create fail"));
        let msg = format!("{err}");
        assert!(msg.contains("create"));
    }

    #[test]
    fn transport_error_display_bind() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 80));
        let err = TransportError::Bind {
            addr,
            source: std::io::Error::other("in use"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("bind"));
    }

    #[test]
    fn transport_error_display_listen() {
        let err = TransportError::Listen(std::io::Error::other("listen fail"));
        let msg = format!("{err}");
        assert!(msg.contains("listen"));
    }

    #[test]
    fn transport_error_display_tokio_convert() {
        let err = TransportError::TokioConvert(std::io::Error::other("convert fail"));
        let msg = format!("{err}");
        assert!(msg.contains("tokio"));
    }

    #[test]
    fn transport_error_display_serve() {
        let err = TransportError::Serve(std::io::Error::other("serve fail"));
        let msg = format!("{err}");
        assert!(msg.contains("serve"));
    }
}
