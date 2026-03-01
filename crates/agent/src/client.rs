use apx_databricks_sdk::DatabricksClient;
use rig::providers::openai;
use tracing::debug;

use crate::error::Result;
use crate::model::{ModelRef, chat_models};

/// Core agent client wrapping a [`DatabricksClient`] for model discovery
/// and rig-based completions.
#[derive(Debug, Clone)]
pub struct AgentClient {
    databricks: DatabricksClient,
}

impl AgentClient {
    /// Create an `AgentClient` from an existing [`DatabricksClient`].
    #[must_use]
    pub const fn new(databricks: DatabricksClient) -> Self {
        Self { databricks }
    }

    /// Create an `AgentClient` by resolving a Databricks CLI profile.
    ///
    /// # Errors
    ///
    /// Returns an error if the profile cannot be resolved.
    pub async fn from_profile(profile: &str) -> Result<Self> {
        let databricks = DatabricksClient::new(profile).await?;
        Ok(Self::new(databricks))
    }

    /// List chat-capable models in the workspace.
    ///
    /// Fetches all serving endpoints and filters to those that are ready
    /// and serve `llm/v1/chat` (or have no task specified).
    ///
    /// # Errors
    ///
    /// Returns an error if the serving endpoints cannot be listed.
    pub async fn list_models(&self) -> Result<Vec<ModelRef>> {
        let endpoints = self.databricks.serving_endpoints().list().await?;
        debug!(count = endpoints.len(), "Fetched serving endpoints");
        Ok(chat_models(&endpoints))
    }

    /// Build a rig [`CompletionsClient`](openai::CompletionsClient) with a fresh
    /// token and the proper base URL for this workspace.
    ///
    /// The base URL is `{host}/serving-endpoints` and the endpoint name is used
    /// as the model ID. The returned client is short-lived — rebuild it when
    /// the token might have expired.
    ///
    /// # Errors
    ///
    /// Returns an error if the token cannot be acquired or the client fails to build.
    pub async fn completions_client(&self) -> Result<openai::CompletionsClient> {
        let token = self.databricks.access_token().await?;
        let base_url = format!("{}/serving-endpoints", self.databricks.host());
        debug!(%base_url, "Building rig CompletionsClient");

        let client = openai::CompletionsClient::builder()
            .api_key(&token)
            .base_url(&base_url)
            .build()
            .map_err(|e| crate::error::AgentError::Completion(e.to_string()))?;

        Ok(client)
    }

    /// Returns a reference to the underlying [`DatabricksClient`].
    #[must_use]
    pub const fn databricks(&self) -> &DatabricksClient {
        &self.databricks
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn mock_list_response() -> serde_json::Value {
        serde_json::json!({
            "endpoints": [
                {
                    "name": "chat-model",
                    "state": { "ready": "READY" },
                    "task": "llm/v1/chat"
                },
                {
                    "name": "embed-model",
                    "state": { "ready": "READY" },
                    "task": "llm/v1/embeddings"
                },
                {
                    "name": "not-ready-model",
                    "state": { "ready": "NOT_READY" },
                    "task": "llm/v1/chat"
                }
            ]
        })
    }

    #[tokio::test]
    async fn list_models_returns_ready_chat_endpoints() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/2.0/serving-endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_response()))
            .mount(&server)
            .await;

        let sdk = DatabricksClient::with_static_token(&server.uri(), "test-token");
        let client = AgentClient::new(sdk);
        let models = client.list_models().await.unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "chat-model");
    }

    #[tokio::test]
    async fn completions_client_builds_successfully() {
        let sdk = DatabricksClient::with_static_token("https://test.databricks.com", "test-token");
        let client = AgentClient::new(sdk);
        let result = client.completions_client().await;
        assert!(result.is_ok());
    }
}
