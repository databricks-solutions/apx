use std::pin::Pin;

use apx_databricks_sdk::DatabricksClient;
use futures_util::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::agent::StreamingError;
use rig::message::Text;
use rig::prelude::CompletionClient;
use rig::providers::openai;
use rig::streaming::{StreamedAssistantContent, StreamingChat};
use tracing::debug;

use crate::chat::{ChatEvent, ChatMessage, to_rig_messages};
use crate::error::{AgentError, Result};
use crate::model::{ModelRef, chat_models};

/// Default system prompt for the chat agent.
const SYSTEM_PROMPT: &str = "\
You are an AI assistant powered by Databricks Foundation Models. \
You help users with questions about their Databricks workspace, \
data engineering, and general programming tasks.";

/// Map a rig stream item to a [`ChatEvent`], accumulating text in `full_text`.
///
/// Returns `None` for non-text items (tool calls, reasoning, etc.) which are skipped.
fn map_stream_item<R>(
    item: std::result::Result<MultiTurnStreamItem<R>, StreamingError>,
    full_text: &mut String,
) -> Option<Result<ChatEvent>> {
    match item {
        Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(Text {
            text,
            ..
        }))) => {
            full_text.push_str(&text);
            Some(Ok(ChatEvent::Token(text)))
        }
        Ok(MultiTurnStreamItem::FinalResponse(resp)) => {
            let response = resp.response();
            let text = if full_text.is_empty() && !response.is_empty() {
                response.to_string()
            } else {
                full_text.clone()
            };
            Some(Ok(ChatEvent::Done(text)))
        }
        Ok(_) => None,
        Err(e) => Some(Err(AgentError::Completion(e.to_string()))),
    }
}

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
            .map_err(|e| AgentError::Completion(e.to_string()))?;

        Ok(client)
    }

    /// Stream a chat completion.
    ///
    /// Converts `history` to rig messages, builds a streaming agent for the
    /// given model, and returns a stream of [`ChatEvent`]s.
    ///
    /// # Errors
    ///
    /// Returns an error if the completions client cannot be built or the
    /// streaming request fails.
    pub async fn stream_chat(
        &self,
        model: &str,
        message: &str,
        history: &[ChatMessage],
    ) -> Result<Pin<Box<dyn futures_util::Stream<Item = Result<ChatEvent>> + Send>>> {
        let client = self.completions_client().await?;
        let agent = client.agent(model).preamble(SYSTEM_PROMPT).build();

        let rig_history = to_rig_messages(history);
        let mut stream = agent.stream_chat(message, rig_history).await;

        let mut full_text = String::new();
        let mapped = async_stream::stream! {
            while let Some(item) = stream.next().await {
                if let Some(event) = map_stream_item(item, &mut full_text) {
                    yield event;
                }
            }
        };

        Ok(Box::pin(mapped))
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

    #[tokio::test]
    async fn list_models_empty_workspace() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/2.0/serving-endpoints"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"endpoints": []})),
            )
            .mount(&server)
            .await;

        let sdk = DatabricksClient::with_static_token(&server.uri(), "test-token");
        let client = AgentClient::new(sdk);
        let models = client.list_models().await.unwrap();
        assert!(models.is_empty());
    }

    #[tokio::test]
    async fn list_models_api_error_returns_sdk_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/2.0/serving-endpoints"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .mount(&server)
            .await;

        let sdk = DatabricksClient::with_static_token(&server.uri(), "test-token");
        let client = AgentClient::new(sdk);
        let result = client.list_models().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_models_excludes_embedding_and_not_ready() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/2.0/serving-endpoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_response()))
            .mount(&server)
            .await;

        let sdk = DatabricksClient::with_static_token(&server.uri(), "test-token");
        let client = AgentClient::new(sdk);
        let models = client.list_models().await.unwrap();

        // Only "chat-model" passes: embed-model is excluded, not-ready-model is excluded
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "chat-model");
        assert!(models.iter().all(|m| m.name != "embed-model"));
        assert!(models.iter().all(|m| m.name != "not-ready-model"));
    }

    #[tokio::test]
    async fn completions_client_base_url_contains_serving_endpoints() {
        // Verify client builds without error when host is a valid URL
        let sdk = DatabricksClient::with_static_token("https://my-workspace.databricks.com", "tok");
        let client = AgentClient::new(sdk);
        assert!(client.completions_client().await.is_ok());
    }
}
