use apx_databricks_sdk::ServingEndpoint;

/// A reference to a model available in the workspace.
#[derive(Debug, Clone)]
pub struct ModelRef {
    /// Serving endpoint name (used as the model ID in completions requests).
    pub name: String,
    /// Task type reported by the endpoint (e.g. `"llm/v1/chat"`).
    pub task: Option<String>,
}

impl ModelRef {
    /// Create a `ModelRef` from a [`ServingEndpoint`].
    #[must_use]
    pub fn from_endpoint(endpoint: &ServingEndpoint) -> Self {
        Self {
            name: endpoint.name.clone(),
            task: endpoint.task.clone(),
        }
    }

    /// Returns `true` if the endpoint serves an `llm/v1/chat` task
    /// (or has no task specified, which is common for custom endpoints).
    #[must_use]
    pub fn is_chat_capable(&self) -> bool {
        matches!(self.task.as_deref(), Some("llm/v1/chat") | None)
    }
}

/// Filter a slice of serving endpoints to those that are READY and chat-capable.
#[must_use]
pub fn chat_models(endpoints: &[ServingEndpoint]) -> Vec<ModelRef> {
    endpoints
        .iter()
        .filter(|ep| ep.is_ready())
        .map(ModelRef::from_endpoint)
        .filter(|m| m.is_chat_capable())
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use apx_databricks_sdk::{EndpointReadyState, EndpointState, ServingEndpoint};

    use super::*;

    fn make_endpoint(name: &str, ready: EndpointReadyState, task: Option<&str>) -> ServingEndpoint {
        ServingEndpoint {
            name: name.to_string(),
            creator: None,
            state: Some(EndpointState {
                ready: Some(ready),
                config_update: None,
            }),
            task: task.map(ToString::to_string),
            config: None,
        }
    }

    #[test]
    fn is_chat_capable_with_chat_task() {
        let m = ModelRef {
            name: "ep".to_string(),
            task: Some("llm/v1/chat".to_string()),
        };
        assert!(m.is_chat_capable());
    }

    #[test]
    fn is_chat_capable_with_no_task() {
        let m = ModelRef {
            name: "ep".to_string(),
            task: None,
        };
        assert!(m.is_chat_capable());
    }

    #[test]
    fn is_chat_capable_with_embeddings_task() {
        let m = ModelRef {
            name: "ep".to_string(),
            task: Some("llm/v1/embeddings".to_string()),
        };
        assert!(!m.is_chat_capable());
    }

    #[test]
    fn chat_models_filters_correctly() {
        let endpoints = vec![
            make_endpoint("chat-ready", EndpointReadyState::Ready, Some("llm/v1/chat")),
            make_endpoint(
                "chat-not-ready",
                EndpointReadyState::NotReady,
                Some("llm/v1/chat"),
            ),
            make_endpoint(
                "embed-ready",
                EndpointReadyState::Ready,
                Some("llm/v1/embeddings"),
            ),
            make_endpoint("no-task-ready", EndpointReadyState::Ready, None),
        ];

        let models = chat_models(&endpoints);
        let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["chat-ready", "no-task-ready"]);
    }
}
