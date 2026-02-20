/// Address for binding servers to all network interfaces.
/// Used by: dev server, uvicorn backend, PGlite, flux agent.
/// This allows connections from any IP, required for Docker/container environments.
pub const BIND_HOST: &str = "0.0.0.0";

/// IPv4 loopback address for local client connections.
/// Used by: health probes, flux OTLP endpoints, OpenAPI fetching, proxy targets.
/// Always use this (not "localhost") to avoid IPv4/IPv6 ambiguity.
pub const CLIENT_HOST: &str = "127.0.0.1";

/// IPv4 loopback as an octet array for std::net::SocketAddr construction.
pub const CLIENT_HOST_OCTETS: [u8; 4] = [127, 0, 0, 1];

/// Hostname for browser-facing URLs and WebSocket connections.
/// "localhost" resolves via DNS and is appropriate for URLs users see in their browser,
/// HMR WebSocket connections, and display messages.
pub const BROWSER_HOST: &str = "localhost";

/// Environment variable name for passing the frontend bind host to entrypoint.ts.
pub const ENV_FRONTEND_HOST: &str = "APX_FRONTEND_HOST";
