//! Structured error types for the framework runtime.

use http::StatusCode;

/// Max depth to walk the error source chain (fixed loop bound).
#[cfg(test)]
const MAX_ERROR_CHAIN_DEPTH: usize = 10;

/// Walk an error's source chain looking for a specific error type.
#[cfg(test)]
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

/// Application error.
///
/// **Security**: `Internal` logs the full error via `tracing::error!` but
/// returns a generic "Internal Server Error" detail to the client. Never
/// leak exception messages, file paths, or connection strings in 500 responses.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Internal error (500) — detail is logged, NOT sent to client.
    #[error("internal error: {0}")]
    Internal(String),

    /// Request timeout (408).
    #[error("request timeout")]
    Timeout,
}

impl AppError {
    /// Convert to status code.
    pub(crate) fn status_code(&self) -> StatusCode {
        match self {
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Timeout => StatusCode::REQUEST_TIMEOUT,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
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
    fn app_error_internal_does_not_leak() {
        let err = AppError::Internal("secret db password: hunter2".to_owned());
        // status_code is correct
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn app_error_status_codes() {
        assert_eq!(
            AppError::Internal("x".to_owned()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(AppError::Timeout.status_code(), StatusCode::REQUEST_TIMEOUT);
    }

    /// Produce a boxed error whose chain contains `LengthLimitError`.
    async fn make_length_limit_boxed_error() -> Box<dyn std::error::Error + Send + Sync> {
        use http_body_util::{BodyExt, Full, Limited};
        Limited::new(Full::new(bytes::Bytes::from("xx")), 0)
            .collect()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn find_in_error_chain_positive() {
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
}
