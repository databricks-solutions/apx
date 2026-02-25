/// Errors returned by the Databricks SDK.
#[derive(Debug, thiserror::Error)]
pub enum DatabricksError {
    /// Authentication failure (e.g. invalid or expired token).
    #[error("authentication error: {0}")]
    Auth(String),
    /// Non-success HTTP response from the Databricks API.
    #[error("API error (HTTP {status}): {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Human-readable error message.
        message: String,
        /// Optional raw response body.
        body: Option<String>,
    },
    /// Configuration error (missing profile, missing host, etc.).
    #[error("configuration error: {0}")]
    Config(String),
    /// Databricks CLI invocation failure.
    #[error("CLI error: {0}")]
    Cli(String),
    /// WebSocket connection or protocol error.
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    /// Input validation error.
    #[error("validation error: {0}")]
    Validation(String),
    /// Underlying HTTP transport error from `reqwest`.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// File-system or I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Convenience alias for `Result<T, DatabricksError>`.
pub type Result<T> = std::result::Result<T, DatabricksError>;
