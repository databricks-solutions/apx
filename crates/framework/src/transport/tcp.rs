//! TCP listener with `SO_REUSEPORT` implementing the [`Listener`] trait.
//!
//! Each worker creates its own `TcpListener` bound to the same port via
//! `SO_REUSEPORT`. The kernel distributes incoming connections across
//! all listeners (on Linux; macOS behavior differs — see spike-results.md).

use super::types::TransportKind;
use super::{Listener, TransportConfig, TransportError};
use std::net::SocketAddr;

/// TCP listener with `SO_REUSEPORT` for multi-worker sharing.
pub struct TcpListener {
    /// The underlying tokio TCP listener.
    inner: tokio::net::TcpListener,
    /// Bound address (resolved after `bind()`).
    addr: SocketAddr,
}

impl std::fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpListener")
            .field("addr", &self.addr)
            .finish()
    }
}

impl TcpListener {
    /// Accept a new TCP connection.
    pub async fn accept(&self) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
        self.inner.accept().await
    }

    /// Expose the inner tokio listener for the hyper service layer (Step 2).
    pub fn into_inner(self) -> tokio::net::TcpListener {
        self.inner
    }
}

impl Listener for TcpListener {
    async fn bind(config: &TransportConfig) -> Result<Self, TransportError>
    where
        Self: Sized,
    {
        let socket = create_socket(config)?;
        let listener = tokio::net::TcpListener::from_std(socket.into())
            .map_err(TransportError::TokioConvert)?;
        let addr = listener.local_addr().map_err(|e| TransportError::Bind {
            addr: SocketAddr::new(config.host, config.port),
            source: e,
        })?;
        Ok(Self {
            inner: listener,
            addr,
        })
    }

    fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    fn transport_kind(&self) -> TransportKind {
        TransportKind::Tcp
    }
}

/// TCP listen backlog — max number of pending connections queued by the kernel.
const LISTEN_BACKLOG: i32 = 1024;

/// Create a `socket2::Socket` configured for `SO_REUSEPORT` TCP listening.
fn create_socket(config: &TransportConfig) -> Result<socket2::Socket, TransportError> {
    let addr = SocketAddr::new(config.host, config.port);
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };

    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
        .map_err(TransportError::SocketCreate)?;

    socket
        .set_reuse_port(true)
        .map_err(TransportError::SocketCreate)?;

    // IPv6 dual-stack: on Linux, set IPV6_V6ONLY=false for dual-stack.
    // macOS handles dual-stack differently — binding "::" already accepts
    // IPv4 connections by default.
    #[cfg(target_os = "linux")]
    if addr.is_ipv6() {
        socket
            .set_only_v6(false)
            .map_err(TransportError::SocketCreate)?;
    }

    socket
        .bind(&addr.into())
        .map_err(|e| TransportError::Bind { addr, source: e })?;

    socket
        .listen(LISTEN_BACKLOG)
        .map_err(TransportError::Listen)?;

    socket
        .set_nonblocking(true)
        .map_err(TransportError::Listen)?;

    Ok(socket)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[tokio::test]
    async fn tcp_listener_bind_ipv4() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await;
        assert!(listener.is_ok(), "IPv4 listener should succeed");
    }

    #[tokio::test]
    async fn tcp_listener_bind_ipv6() {
        let config = TransportConfig::tcp(IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await;
        assert!(listener.is_ok(), "IPv6 listener should succeed");
    }

    #[tokio::test]
    async fn tcp_listener_local_addr() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await;
        assert!(listener.is_ok());
        let listener = listener.unwrap_or_else(|_| unreachable!());
        assert_ne!(listener.local_addr().port(), 0);
    }

    #[tokio::test]
    async fn tcp_listener_transport_kind() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await;
        assert!(listener.is_ok());
        let listener = listener.unwrap_or_else(|_| unreachable!());
        assert_eq!(listener.transport_kind(), TransportKind::Tcp);
    }

    #[tokio::test]
    async fn tcp_listener_debug() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let dbg = format!("{listener:?}");
        assert!(dbg.contains("TcpListener"));
        assert!(dbg.contains("addr"));
    }

    #[tokio::test]
    async fn tcp_listener_accept_returns_connection() {
        let config = TransportConfig::tcp(IpAddr::from([127, 0, 0, 1]), 0);
        let listener = TcpListener::bind(&config).await.unwrap();
        let addr = listener.local_addr();

        // Connect from a client
        let _client = tokio::net::TcpStream::connect(addr).await.unwrap();

        // Accept should succeed
        let (stream, client_addr) = listener.accept().await.unwrap();
        assert!(stream.peer_addr().is_ok());
        assert_eq!(client_addr.ip(), IpAddr::from([127, 0, 0, 1]));
    }
}
