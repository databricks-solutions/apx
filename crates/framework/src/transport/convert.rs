//! Bidirectional conversion between axum types and transport-neutral types.
//!
//! This module is the **only place** where axum's `Request`/`Response`/`Body`
//! types cross the transport boundary. Keeping these conversions isolated
//! ensures the application layer never depends on axum's HTTP types directly.
//!
//! ## Phase 2 deliverable
//!
//! - `from_axum_request()`: `axum::extract::Request` → `InboundRequest`
//! - `to_axum_response()`: `OutboundResponse` → `axum::response::Response`
//! - `body_stream_from_axum()`: `axum::body::Body` → `BodyStream`
