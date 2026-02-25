//! Network host constants for binding, client connections, and browser URLs.

/// Default bind address for all APX services (dev server, uvicorn, `PGlite`, flux).
/// Loopback-only to prevent LAN exposure. Override with `--host 0.0.0.0` if needed.
pub const BIND_HOST: &str = "127.0.0.1";

/// IPv4 loopback address for local client connections.
/// Used by: health probes, flux OTLP endpoints, `OpenAPI` fetching, proxy targets.
/// Always use this (not "localhost") to avoid IPv4/IPv6 ambiguity.
pub const CLIENT_HOST: &str = "127.0.0.1";

/// IPv4 loopback as an octet array for `std::net::SocketAddr` construction.
pub const CLIENT_HOST_OCTETS: [u8; 4] = [127, 0, 0, 1];

/// Hostname for browser-facing URLs and WebSocket connections.
/// "localhost" resolves via DNS and is appropriate for URLs users see in their browser,
/// HMR WebSocket connections, and display messages.
pub const BROWSER_HOST: &str = "localhost";

/// Environment variable name for passing the frontend bind host to entrypoint.ts.
pub const ENV_FRONTEND_HOST: &str = "APX_FRONTEND_HOST";
