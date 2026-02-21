use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::schemars;

/// Marker trait for types that serialize to a JSON **object**.
///
/// The MCP spec requires `structuredContent` to be a record (JSON object),
/// not an array or scalar.  Implement this on every tool response struct so
/// that `from_serializable` enforces the constraint at compile time — passing
/// a `Vec<T>`, `String`, `i64`, etc. will fail to compile.
pub trait StructuredObject: serde::Serialize {}

/// Declares a tool-response struct with `#[derive(Serialize)]` and
/// `impl StructuredObject` in one shot, so new tools can't forget the marker.
macro_rules! tool_response {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $field_vis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(serde::Serialize)]
        $vis struct $name {
            $(
                $(#[$field_meta])*
                $field_vis $field : $ty,
            )*
        }
        impl $crate::tools::StructuredObject for $name {}
    };
}

// Submodules declared after macro so they can use `tool_response!`.
pub mod databricks;
pub mod devserver;
pub mod docs;
pub mod feedback;
pub mod project;
pub mod registry;

/// Shared args for tools that only need an app path.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AppPathArgs {
    /// Absolute path to the project directory
    pub app_path: String,
}

/// Extension trait for building `CallToolResult` from serializable values.
pub trait ToolResultExt {
    fn from_serializable(value: &impl StructuredObject) -> Self;
    fn from_serializable_error(value: &impl StructuredObject) -> Self;
}

/// Shared serialization logic for `from_serializable` / `from_serializable_error`.
fn build_structured_result(value: &impl StructuredObject, is_error: bool) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(v) => {
            let text_fallback = serde_json::to_string_pretty(&v).unwrap_or_default();
            CallToolResult {
                content: vec![Content::text(text_fallback)],
                structured_content: Some(v),
                is_error: Some(is_error),
                meta: None,
            }
        }
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "Failed to serialize response: {e}"
        ))]),
    }
}

impl ToolResultExt for CallToolResult {
    fn from_serializable(value: &impl StructuredObject) -> Self {
        build_structured_result(value, false)
    }

    fn from_serializable_error(value: &impl StructuredObject) -> Self {
        build_structured_result(value, true)
    }
}
