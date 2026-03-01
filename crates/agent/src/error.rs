use apx_databricks_sdk::DatabricksError;

/// Errors returned by the agent crate.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// An error from the Databricks SDK layer.
    #[error(transparent)]
    Sdk(#[from] DatabricksError),
    /// An HTTP transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// The requested model was not found in the workspace.
    #[error("model not found: {0}")]
    ModelNotFound(String),
    /// A completion request failed.
    #[error("completion error: {0}")]
    Completion(String),
}

/// Convenience alias for `Result<T, AgentError>`.
pub type Result<T> = std::result::Result<T, AgentError>;
