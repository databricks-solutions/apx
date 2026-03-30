//! Transport listener trait and configuration.

use super::types::TransportKind;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};

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

/// Configuration for transport binding.
#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    /// IP address to bind.
    pub host: IpAddr,
    /// Port to bind.
    pub port: u16,
    /// Which transport to use.
    pub transport_kind: TransportKind,
}

impl TransportConfig {
    /// Create a TCP transport config.
    pub fn tcp(host: IpAddr, port: u16) -> Self {
        Self {
            host,
            port,
            transport_kind: TransportKind::Tcp,
        }
    }
}

/// Transport-agnostic listener trait.
///
/// v1: `TcpListener` (hyper for HTTP/1 + HTTP/2).
/// Future: `QuicListener` (quinn for HTTP/3), `UnixListener`, `InMemoryListener`.
///
/// The `serve()` method is intentionally absent — it lives in the hyper service
/// layer (`protocol::http::service`).
pub trait Listener: Send + Sync + 'static {
    /// Bind to the configured address.
    fn bind(config: &TransportConfig) -> impl Future<Output = Result<Self, TransportError>> + Send
    where
        Self: Sized;

    /// Return the locally bound socket address.
    fn local_addr(&self) -> SocketAddr;

    /// Return the transport kind.
    fn transport_kind(&self) -> TransportKind;
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn transport_config_tcp() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 8080);
        assert_eq!(config.host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(config.port, 8080);
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
