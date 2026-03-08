//! Structured error types and RFC 9457 problem detail responses.
//!
//! All HTTP error responses are formatted as `application/problem+json`
//! per [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457). Internal errors
//! never leak implementation details to clients.

use crate::route::ProblemTypeUri;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::Serialize;

// ── RFC 9457 problem detail ─────────────────────────────────────────────

/// RFC 9457 problem detail response body.
///
/// The `type` field is a URI reference per RFC 9457 §3.1.1, defaulting to
/// `"about:blank"`.
#[derive(Debug, Serialize)]
pub struct ProblemDetail {
    /// Problem type URI.
    pub r#type: ProblemTypeUri,
    /// Short human-readable summary.
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// Detailed human-readable explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// URI reference identifying the specific occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Structured validation errors (only for 422 responses).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ValidationErrorItem>,
}

/// Structured Pydantic validation error — safe to include in response.
///
/// Mirrors Pydantic's `ValidationError.errors()` output.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationErrorItem {
    /// Location path: `["body", "name"]`.
    pub loc: Vec<String>,
    /// Human-readable message: `"Field required"`.
    pub msg: String,
    /// Error type code: `"missing"`.
    pub r#type: String,
}

// ── Body parse failure ──────────────────────────────────────────────────

/// Body parse failure — fixed set of safe messages, never raw error strings.
///
/// Mapped from actual error types, NOT string matching.
#[derive(Debug, Clone, Copy)]
pub enum BodyParseKind {
    /// JSON syntax error.
    InvalidJson,
    /// Request body exceeded the configured limit.
    BodyTooLarge,
    /// Content-Type is not `application/json`.
    UnsupportedContentType,
}

impl BodyParseKind {
    /// Human-readable detail message for the client.
    fn detail(self) -> &'static str {
        match self {
            Self::InvalidJson => "Request body is not valid JSON",
            Self::BodyTooLarge => "Request body exceeds the maximum allowed size",
            Self::UnsupportedContentType => "Content-Type must be application/json",
        }
    }
}

// ── Error chain walker ──────────────────────────────────────────────────

/// Max depth to walk the error source chain (fixed loop bound).
///
/// hyper/axum chains are typically 2–3 deep; 10 is a generous safety margin.
const MAX_ERROR_CHAIN_DEPTH: usize = 10;

/// Walk an error's source chain looking for a specific error type.
///
/// Generic, reusable, testable with synthetic error chains.
pub fn find_in_error_chain<T: std::error::Error + 'static>(
    err: &dyn std::error::Error,
) -> Option<&T> {
    let mut source = err.source();
    for _ in 0..MAX_ERROR_CHAIN_DEPTH {
        let e = source?;
        if let Some(found) = e.downcast_ref::<T>() {
            return Some(found);
        }
        source = e.source();
    }
    None
}

// ── Application error enum ──────────────────────────────────────────────

/// Application error that converts to RFC 9457 responses.
///
/// **Security**: `Internal` logs the full error via `tracing::error!` but
/// returns a generic "Internal Server Error" detail to the client. Never
/// leak exception messages, file paths, or connection strings in 500 responses.
///
/// **Security**: `Validation` carries structured error items from Pydantic,
/// not raw strings. `BodyParse` uses a fixed enum — never forwards raw
/// hyper/axum error text.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Pydantic validation failed (422) — structured, safe to return.
    #[error("validation error")]
    Validation(Vec<ValidationErrorItem>),

    /// Python exception in handler (500) — detail is logged, NOT sent to client.
    #[error("internal error")]
    Internal(String),

    /// Body parse error (400) — fixed messages only.
    #[error("body parse error")]
    BodyParse(BodyParseKind),

    /// Request timeout (408) — tower `TimeoutLayer` fires.
    #[error("request timeout")]
    Timeout,
}

impl AppError {
    /// Convert to status code.
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BodyParse(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Timeout => StatusCode::REQUEST_TIMEOUT,
        }
    }

    /// Convert to RFC 9457 problem detail title.
    fn title(&self) -> &'static str {
        match self {
            Self::Validation(_) => "Validation Error",
            Self::BodyParse(_) => "Bad Request",
            Self::Internal(_) => "Internal Server Error",
            Self::Timeout => "Request Timeout",
        }
    }

    /// Convert to client-safe detail message.
    fn client_detail(&self) -> Option<String> {
        match self {
            Self::BodyParse(kind) => Some(kind.detail().to_owned()),
            Self::Timeout => Some("The request exceeded the allowed processing time".to_owned()),
            Self::Internal(_) => Some("An unexpected error occurred".to_owned()),
            Self::Validation(_) => Some("Request validation failed".to_owned()),
        }
    }
}

/// Content-Type header value for RFC 9457 problem detail responses.
const PROBLEM_JSON_CONTENT_TYPE: &str = "application/problem+json";

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let title = self.title();
        let detail = self.client_detail();

        // Destructure to take ownership — avoids cloning validation items.
        let errors = match self {
            Self::Internal(ref msg) => {
                tracing::error!(error = %msg, "handler returned internal error");
                Vec::new()
            }
            Self::Validation(items) => items,
            _ => Vec::new(),
        };

        let problem = ProblemDetail {
            r#type: ProblemTypeUri::blank(),
            title: title.to_owned(),
            status: status.as_u16(),
            detail,
            instance: None,
            errors,
        };

        let body = serde_json::to_vec(&problem).unwrap_or_else(|_| {
            // Fallback: this should never happen since ProblemDetail is always
            // serializable, but handle gracefully.
            r#"{"type":"about:blank","title":"Internal Server Error","status":500}"#
                .as_bytes()
                .to_vec()
        });

        Response::builder()
            .status(status)
            .header(http::header::CONTENT_TYPE, PROBLEM_JSON_CONTENT_TYPE)
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| {
                // This can only fail if the status code or headers are invalid,
                // which they aren't — the values are hardcoded.
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            })
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::*;

    #[test]
    fn find_in_error_chain_not_found() {
        #[derive(Debug)]
        struct SimpleErr;
        impl std::fmt::Display for SimpleErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("simple")
            }
        }
        impl std::error::Error for SimpleErr {}

        let err = SimpleErr;
        assert!(find_in_error_chain::<http_body_util::LengthLimitError>(&err).is_none());
    }

    #[test]
    fn body_parse_kind_messages_are_safe() {
        // Verify messages don't contain implementation details.
        let msg = BodyParseKind::InvalidJson.detail();
        assert!(!msg.contains("hyper"));
        assert!(!msg.contains("axum"));
    }

    #[test]
    fn app_error_internal_does_not_leak() {
        let err = AppError::Internal("secret db password: hunter2".to_owned());
        let detail = err.client_detail();
        assert_eq!(detail.as_deref(), Some("An unexpected error occurred"));
    }

    #[test]
    fn app_error_status_codes() {
        assert_eq!(
            AppError::Validation(vec![]).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            AppError::Internal("x".to_owned()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(AppError::Timeout.status_code(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            AppError::BodyParse(BodyParseKind::BodyTooLarge).status_code(),
            StatusCode::BAD_REQUEST
        );
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// Produce a boxed error whose chain contains `LengthLimitError`.
    /// The struct is `#[non_exhaustive]` so it cannot be constructed directly.
    async fn make_length_limit_boxed_error() -> Box<dyn std::error::Error + Send + Sync> {
        use http_body_util::{BodyExt, Full, Limited};
        Limited::new(Full::new(bytes::Bytes::from("xx")), 0)
            .collect()
            .await
            .unwrap_err()
    }

    // ── find_in_error_chain positive ─────────────────────────────────────

    #[tokio::test]
    async fn find_in_error_chain_positive() {
        // The boxed error IS a LengthLimitError — wrap it so `source()` yields it.
        #[derive(Debug)]
        struct Wrapper(Box<dyn std::error::Error + Send + Sync>);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("wrap")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self.0.as_ref())
            }
        }
        let lle = make_length_limit_boxed_error().await;
        let err = Wrapper(lle);
        assert!(find_in_error_chain::<http_body_util::LengthLimitError>(&err).is_some());
    }

    #[tokio::test]
    async fn find_in_error_chain_depth_two() {
        #[derive(Debug)]
        struct Inner(Box<dyn std::error::Error + Send + Sync>);
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("inner")
            }
        }
        impl std::error::Error for Inner {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self.0.as_ref())
            }
        }

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let lle = make_length_limit_boxed_error().await;
        let err = Outer(Inner(lle));
        assert!(find_in_error_chain::<http_body_util::LengthLimitError>(&err).is_some());
    }

    // ── BodyParseKind::detail ────────────────────────────────────────────

    #[test]
    fn body_parse_kind_detail_too_large() {
        let msg = BodyParseKind::BodyTooLarge.detail();
        assert!(msg.contains("maximum allowed size"));
    }

    #[test]
    fn body_parse_kind_detail_unsupported_content_type() {
        let msg = BodyParseKind::UnsupportedContentType.detail();
        assert!(msg.contains("application/json"));
    }

    // ── client_detail ────────────────────────────────────────────────────

    #[test]
    fn client_detail_timeout() {
        let detail = AppError::Timeout.client_detail();
        assert!(detail.unwrap().contains("processing time"));
    }

    #[test]
    fn client_detail_validation() {
        let detail = AppError::Validation(vec![]).client_detail();
        assert!(detail.unwrap().contains("validation failed"));
    }

    #[test]
    fn client_detail_body_parse_variants() {
        let detail = AppError::BodyParse(BodyParseKind::InvalidJson).client_detail();
        assert!(detail.unwrap().contains("not valid JSON"));

        let detail = AppError::BodyParse(BodyParseKind::BodyTooLarge).client_detail();
        assert!(detail.unwrap().contains("maximum allowed size"));

        let detail = AppError::BodyParse(BodyParseKind::UnsupportedContentType).client_detail();
        assert!(detail.unwrap().contains("application/json"));
    }

    // ── IntoResponse ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn into_response_internal() {
        use axum::response::IntoResponse;
        let resp = AppError::Internal("secret".to_owned()).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 500);
        // Must not leak the internal detail
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(!body_str.contains("secret"));
    }

    #[tokio::test]
    async fn into_response_timeout() {
        use axum::response::IntoResponse;
        let resp = AppError::Timeout.into_response();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 408);
    }

    #[tokio::test]
    async fn into_response_validation() {
        use axum::response::IntoResponse;
        let resp = AppError::Validation(vec![ValidationErrorItem {
            loc: vec!["body".to_owned(), "name".to_owned()],
            msg: "Field required".to_owned(),
            r#type: "missing".to_owned(),
        }])
        .into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 422);
        assert!(!json["errors"].as_array().unwrap().is_empty());
    }
}
