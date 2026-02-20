pub mod databricks;
pub mod devserver;
pub mod docs;
pub mod feedback;
pub mod project;
pub mod registry;

use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::schemars;

/// Shared args for tools that only need an app path.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AppPathArgs {
    /// Absolute path to the project directory
    pub app_path: String,
}

/// Extension trait for building `CallToolResult` from serializable values.
pub trait ToolResultExt {
    fn from_serializable(value: &impl serde::Serialize) -> Self;
}

impl ToolResultExt for CallToolResult {
    fn from_serializable(value: &impl serde::Serialize) -> Self {
        match serde_json::to_value(value) {
            Ok(v) => {
                let text_fallback = serde_json::to_string_pretty(&v).unwrap_or_default();
                CallToolResult {
                    content: vec![Content::text(text_fallback)],
                    structured_content: Some(v),
                    is_error: Some(false),
                    meta: None,
                }
            }
            Err(e) => CallToolResult::error(vec![Content::text(format!(
                "Failed to serialize response: {e}"
            ))]),
        }
    }
}
