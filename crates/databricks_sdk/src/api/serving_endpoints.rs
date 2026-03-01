use serde::Deserialize;

use crate::client::DatabricksClient;
use crate::error::Result;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Private envelope for deserializing the list response.
#[derive(Deserialize)]
struct ListResponse {
    #[serde(default)]
    endpoints: Vec<ServingEndpoint>,
}

/// Readiness state of a serving endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndpointReadyState {
    /// The endpoint is ready to serve requests.
    Ready,
    /// The endpoint is not yet ready.
    NotReady,
    /// An unrecognized state (forward-compatible).
    #[serde(other)]
    Unknown,
}

/// Config-update state of a serving endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndpointConfigUpdateState {
    /// No config update in progress.
    NotUpdating,
    /// A config update is in progress.
    InProgress,
    /// A config update was canceled.
    UpdateCanceled,
    /// A config update failed.
    UpdateFailed,
    /// An unrecognized state (forward-compatible).
    #[serde(other)]
    Unknown,
}

/// Nested state object of a serving endpoint.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct EndpointState {
    /// Whether the endpoint is ready to serve.
    #[serde(default)]
    pub ready: Option<EndpointReadyState>,
    /// Current config-update status.
    #[serde(default)]
    pub config_update: Option<EndpointConfigUpdateState>,
}

/// An external model reference within a served entity.
#[derive(Debug, Clone, Deserialize)]
pub struct ExternalModel {
    /// Model name (e.g. `"gpt-4"`).
    pub name: String,
    /// Provider name (e.g. `"openai"`).
    #[serde(default)]
    pub provider: Option<String>,
}

/// A single served entity (model) within a serving endpoint config.
#[derive(Debug, Clone, Deserialize)]
pub struct ServedEntity {
    /// Name of the served entity.
    #[serde(default)]
    pub name: Option<String>,
    /// External model reference, if this entity wraps an external model.
    #[serde(default)]
    pub external_model: Option<ExternalModel>,
}

/// Configuration of a serving endpoint, containing the served entities.
#[derive(Debug, Clone, Deserialize)]
pub struct EndpointConfig {
    /// Current served entities (preferred).
    #[serde(default)]
    pub served_entities: Vec<ServedEntity>,
    /// Deprecated alias for `served_entities`.
    #[serde(default)]
    pub served_models: Vec<ServedEntity>,
}

/// A Databricks serving endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ServingEndpoint {
    /// Endpoint name (unique within the workspace).
    pub name: String,
    /// User who created the endpoint.
    #[serde(default)]
    pub creator: Option<String>,
    /// Current endpoint state (readiness, config update status).
    #[serde(default)]
    pub state: Option<EndpointState>,
    /// Task type (e.g. `"llm/v1/chat"`, `"llm/v1/completions"`).
    #[serde(default)]
    pub task: Option<String>,
    /// Endpoint configuration with served entities.
    #[serde(default)]
    pub config: Option<EndpointConfig>,
}

impl ServingEndpoint {
    /// Returns `true` if the endpoint is in the `Ready` state.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state
            .as_ref()
            .and_then(|s| s.ready)
            .is_some_and(|r| r == EndpointReadyState::Ready)
    }

    /// Returns the served entities, preferring `served_entities` over the
    /// deprecated `served_models` field.
    #[must_use]
    pub fn entities(&self) -> &[ServedEntity] {
        match self.config {
            Some(ref cfg) if !cfg.served_entities.is_empty() => &cfg.served_entities,
            Some(ref cfg) => &cfg.served_models,
            None => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// API handle
// ---------------------------------------------------------------------------

/// API handle for Databricks Serving Endpoints operations.
#[derive(Debug)]
pub struct ServingEndpointsApi<'a> {
    client: &'a DatabricksClient,
}

impl<'a> ServingEndpointsApi<'a> {
    pub(crate) const fn new(client: &'a DatabricksClient) -> Self {
        Self { client }
    }

    /// List all serving endpoints in the workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the response cannot be deserialized.
    pub async fn list(&self) -> Result<Vec<ServingEndpoint>> {
        let resp: ListResponse = self.client.get("/api/2.0/serving-endpoints").await?;
        Ok(resp.endpoints)
    }

    /// Get a single serving endpoint by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the response cannot be deserialized.
    pub async fn get(&self, name: &str) -> Result<ServingEndpoint> {
        self.client
            .get(&format!("/api/2.0/serving-endpoints/{name}"))
            .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_endpoint_ready() {
        let json = r#"{
            "name": "my-endpoint",
            "creator": "user@example.com",
            "state": { "ready": "READY", "config_update": "NOT_UPDATING" },
            "task": "llm/v1/chat",
            "config": {
                "served_entities": [{"name": "entity-1"}],
                "served_models": []
            }
        }"#;
        let ep: ServingEndpoint = serde_json::from_str(json).unwrap();
        assert_eq!(ep.name, "my-endpoint");
        assert_eq!(ep.creator.as_deref(), Some("user@example.com"));
        assert!(ep.is_ready());
        assert_eq!(ep.task.as_deref(), Some("llm/v1/chat"));
        assert_eq!(ep.entities().len(), 1);
    }

    #[test]
    fn deserialize_endpoint_not_ready() {
        let json = r#"{
            "name": "ep-2",
            "state": { "ready": "NOT_READY" }
        }"#;
        let ep: ServingEndpoint = serde_json::from_str(json).unwrap();
        assert!(!ep.is_ready());
    }

    #[test]
    fn deserialize_endpoint_unknown_state() {
        let json = r#"{
            "name": "ep-3",
            "state": { "ready": "SOME_FUTURE_STATE" }
        }"#;
        let ep: ServingEndpoint = serde_json::from_str(json).unwrap();
        assert!(!ep.is_ready());
        assert_eq!(
            ep.state.as_ref().unwrap().ready,
            Some(EndpointReadyState::Unknown)
        );
    }

    #[test]
    fn deserialize_endpoint_minimal() {
        let json = r#"{ "name": "bare-ep" }"#;
        let ep: ServingEndpoint = serde_json::from_str(json).unwrap();
        assert_eq!(ep.name, "bare-ep");
        assert!(ep.creator.is_none());
        assert!(ep.state.is_none());
        assert!(ep.task.is_none());
        assert!(ep.config.is_none());
        assert!(!ep.is_ready());
        assert!(ep.entities().is_empty());
    }

    #[test]
    fn deserialize_list_response_empty() {
        let json = r#"{ "endpoints": [] }"#;
        let resp: ListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.endpoints.is_empty());
    }

    #[test]
    fn deserialize_list_response_missing_endpoints_key() {
        let json = r#"{}"#;
        let resp: ListResponse = serde_json::from_str(json).unwrap();
        assert!(resp.endpoints.is_empty());
    }

    #[test]
    fn is_ready_true_when_ready() {
        let ep = ServingEndpoint {
            name: "test".to_string(),
            creator: None,
            state: Some(EndpointState {
                ready: Some(EndpointReadyState::Ready),
                config_update: None,
            }),
            task: None,
            config: None,
        };
        assert!(ep.is_ready());
    }

    #[test]
    fn is_ready_false_when_not_ready() {
        let ep = ServingEndpoint {
            name: "test".to_string(),
            creator: None,
            state: Some(EndpointState {
                ready: Some(EndpointReadyState::NotReady),
                config_update: None,
            }),
            task: None,
            config: None,
        };
        assert!(!ep.is_ready());
    }

    #[test]
    fn entities_prefers_served_entities() {
        let ep = ServingEndpoint {
            name: "test".to_string(),
            creator: None,
            state: None,
            task: None,
            config: Some(EndpointConfig {
                served_entities: vec![ServedEntity {
                    name: Some("primary".to_string()),
                    external_model: None,
                }],
                served_models: vec![ServedEntity {
                    name: Some("fallback".to_string()),
                    external_model: None,
                }],
            }),
        };
        assert_eq!(ep.entities().len(), 1);
        assert_eq!(ep.entities()[0].name.as_deref(), Some("primary"));
    }

    #[test]
    fn entities_falls_back_to_served_models() {
        let ep = ServingEndpoint {
            name: "test".to_string(),
            creator: None,
            state: None,
            task: None,
            config: Some(EndpointConfig {
                served_entities: vec![],
                served_models: vec![ServedEntity {
                    name: Some("legacy".to_string()),
                    external_model: None,
                }],
            }),
        };
        assert_eq!(ep.entities().len(), 1);
        assert_eq!(ep.entities()[0].name.as_deref(), Some("legacy"));
    }
}
