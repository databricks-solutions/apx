#[derive(Debug, thiserror::Error)]
pub enum DatabricksError {
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("API error (HTTP {status}): {message}")]
    Api {
        status: u16,
        message: String,
        body: Option<String>,
    },
    #[error("configuration error: {0}")]
    Config(String),
    #[error("CLI error: {0}")]
    Cli(String),
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, DatabricksError>;
