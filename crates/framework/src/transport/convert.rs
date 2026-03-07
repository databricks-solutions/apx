//! Bidirectional conversion between axum types and transport-neutral types.
//!
//! This module is the **only place** where axum's `Request`/`Response`/`Body`
//! types cross the transport boundary. Keeping these conversions isolated
//! ensures the application layer never depends on axum's HTTP types directly.

use crate::transport::types::{
    BodyStream, InboundRequest, OutboundResponse, ProtocolVersion, ResponseBody, TransportKind,
};
use bytes::Bytes;
use std::net::SocketAddr;
use tokio_stream::StreamExt;

/// Convert an axum `Body` into a transport-neutral [`BodyStream`].
///
/// Lives here (not on `BodyStream`) to keep axum types out of `transport/types.rs`.
fn body_stream_from_axum(body: axum::body::Body) -> BodyStream {
    let stream = http_body_util::BodyStream::new(body).filter_map(|result| match result {
        Ok(frame) => frame.into_data().ok().map(Ok),
        Err(e) => Some(Err(std::io::Error::other(e))),
    });
    BodyStream::Stream(Box::pin(stream))
}

/// Convert an `axum::extract::Request` to [`InboundRequest`].
///
/// Called once per request in the axum handler. This is the sole point where
/// axum types are consumed and transport-neutral types are produced.
pub fn from_axum_request(
    request: axum::extract::Request,
    path_params: Vec<(String, String)>,
    server_addr: SocketAddr,
    client_addr: Option<SocketAddr>,
) -> InboundRequest {
    let (parts, body) = request.into_parts();

    let protocol = match parts.version {
        http::Version::HTTP_10 => ProtocolVersion::Http10,
        http::Version::HTTP_2 => ProtocolVersion::H2,
        // HTTP/1.1 is the conservative default for both known and unknown versions.
        _ => ProtocolVersion::Http11,
    };

    let query_string = parts
        .uri
        .query()
        .map(|q| Bytes::copy_from_slice(q.as_bytes()))
        .unwrap_or_default();

    let path = parts.uri.path().to_owned();

    InboundRequest::new(
        parts.method,
        path,
        query_string,
        parts.headers,
        body_stream_from_axum(body),
        protocol,
        TransportKind::Tcp,
        client_addr,
        server_addr,
        path_params,
        parts.extensions,
    )
}

/// Convert an [`OutboundResponse`] to `axum::response::Response`.
///
/// Handles both fixed and streaming response bodies.
///
/// # Panics
///
/// Panics if the response builder fails. This cannot happen because status
/// and headers originate from validated `http` types.
pub fn to_axum_response(response: OutboundResponse) -> axum::response::Response {
    let mut builder = axum::response::Response::builder().status(response.status);

    for (name, value) in &response.headers {
        builder = builder.header(name, value);
    }

    let body = match response.body {
        ResponseBody::Fixed(bytes) => axum::body::Body::from(bytes),
        ResponseBody::Stream(stream) => axum::body::Body::from_stream(stream),
    };

    // SAFETY: builder cannot fail — status is a valid StatusCode and headers
    // come from a validated HeaderMap. Both are pre-constructed http types.
    #[expect(
        clippy::expect_used,
        reason = "proven invariant: pre-validated http types"
    )]
    builder
        .body(body)
        .expect("pre-validated status and headers cannot produce invalid response")
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;
    use http::header::HeaderMap;

    #[test]
    fn from_axum_request_basic() {
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();

        let inbound = from_axum_request(
            req,
            Vec::new(),
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            Some(SocketAddr::from(([10, 0, 0, 1], 5555))),
        );

        assert_eq!(inbound.method, http::Method::GET);
        assert_eq!(inbound.path, "/");
        assert!(inbound.query_string.is_empty());
        assert_eq!(inbound.protocol, ProtocolVersion::Http11);
        assert_eq!(inbound.transport, TransportKind::Tcp);
        assert_eq!(
            inbound.server_addr,
            SocketAddr::from(([127, 0, 0, 1], 8080))
        );
        assert_eq!(
            inbound.client_addr,
            Some(SocketAddr::from(([10, 0, 0, 1], 5555)))
        );
    }

    #[test]
    fn from_axum_request_preserves_headers() {
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/api")
            .header("content-type", "application/json")
            .header("x-request-id", "abc-123")
            .body(axum::body::Body::empty())
            .unwrap();

        let inbound = from_axum_request(
            req,
            Vec::new(),
            SocketAddr::from(([0, 0, 0, 0], 3000)),
            None,
        );

        assert_eq!(
            inbound.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(inbound.headers.get("x-request-id").unwrap(), "abc-123");
    }

    #[test]
    fn from_axum_request_extracts_query_string() {
        let req = http::Request::builder()
            .uri("/search?q=hello&page=2")
            .body(axum::body::Body::empty())
            .unwrap();

        let inbound = from_axum_request(
            req,
            Vec::new(),
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            None,
        );

        assert_eq!(inbound.query_string.as_ref(), b"q=hello&page=2");
    }

    #[test]
    fn from_axum_request_protocol_versions() {
        for (version, expected) in [
            (http::Version::HTTP_10, ProtocolVersion::Http10),
            (http::Version::HTTP_11, ProtocolVersion::Http11),
            (http::Version::HTTP_2, ProtocolVersion::H2),
        ] {
            let req = http::Request::builder()
                .version(version)
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap();

            let inbound = from_axum_request(
                req,
                Vec::new(),
                SocketAddr::from(([127, 0, 0, 1], 8080)),
                None,
            );

            assert_eq!(inbound.protocol, expected, "version {version:?}");
        }
    }

    #[test]
    fn from_axum_request_with_path_params() {
        let req = http::Request::builder()
            .uri("/items/42")
            .body(axum::body::Body::empty())
            .unwrap();

        let params = vec![("item_id".to_owned(), "42".to_owned())];
        let inbound =
            from_axum_request(req, params, SocketAddr::from(([127, 0, 0, 1], 8080)), None);

        assert_eq!(
            inbound.path_params,
            vec![("item_id".to_owned(), "42".to_owned())]
        );
    }

    #[tokio::test]
    async fn to_axum_response_fixed_body() {
        let response = OutboundResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBody::Fixed(Bytes::from("hello")),
        };

        let resp = to_axum_response(response);
        assert_eq!(resp.status(), http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"hello");
    }

    #[tokio::test]
    async fn to_axum_response_streaming_body() {
        let chunks = vec![Ok(Bytes::from("hel")), Ok(Bytes::from("lo"))];
        let stream = tokio_stream::iter(chunks);
        let response = OutboundResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBody::Stream(Box::pin(stream)),
        };

        let resp = to_axum_response(response);
        assert_eq!(resp.status(), http::StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"hello");
    }

    #[tokio::test]
    async fn to_axum_response_preserves_status_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let response = OutboundResponse {
            status: http::StatusCode::CREATED,
            headers,
            body: ResponseBody::Fixed(Bytes::from("{}")),
        };

        let resp = to_axum_response(response);
        assert_eq!(resp.status(), http::StatusCode::CREATED);
        assert_eq!(resp.headers().get("x-custom").unwrap(), "value");
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn roundtrip_status_and_headers() {
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/api/items?sort=name")
            .header("accept", "application/json")
            .body(axum::body::Body::empty())
            .unwrap();

        let inbound = from_axum_request(
            req,
            vec![("id".to_owned(), "7".to_owned())],
            SocketAddr::from(([127, 0, 0, 1], 8080)),
            Some(SocketAddr::from(([10, 0, 0, 1], 9999))),
        );

        assert_eq!(inbound.method, http::Method::POST);
        assert_eq!(inbound.path, "/api/items");
        assert_eq!(inbound.query_string.as_ref(), b"sort=name");
        assert_eq!(inbound.headers.get("accept").unwrap(), "application/json");

        let mut resp_headers = HeaderMap::new();
        resp_headers.insert("content-type", "application/json".parse().unwrap());

        let outbound = OutboundResponse {
            status: http::StatusCode::CREATED,
            headers: resp_headers,
            body: ResponseBody::Fixed(Bytes::from(r#"{"id":7}"#)),
        };

        let resp = to_axum_response(outbound);
        assert_eq!(resp.status(), http::StatusCode::CREATED);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"id":7}"#);
    }
}
