use rmcp::model::{CallToolResult, Content};

/// Domain errors for MCP tool handlers.
///
/// Separates *protocol-level* errors (bad input from the caller) from
/// *tool-level* errors (operation couldn't complete but the request was valid).
#[derive(Debug)]
pub enum ToolError {
    /// Bad input → `Err(ErrorData::invalid_params)` — protocol-level.
    InvalidInput(String),
    /// Missing config (e.g. no `[tool.apx.ui]`) → `Ok(CallToolResult::error)` — tool-level.
    NotConfigured(String),
    /// External operation failure → `Ok(CallToolResult::error)` — tool-level.
    OperationFailed(String),
    /// Index still building → `Ok(CallToolResult::error)` — tool-level.
    IndexNotReady(String),
}

impl ToolError {
    /// Convert into the MCP result type.
    ///
    /// `InvalidInput` becomes a protocol-level `Err` (the request itself was
    /// malformed).  All other variants become a successful protocol response
    /// whose *tool* result carries `is_error = true`.
    pub fn into_result(self) -> Result<CallToolResult, rmcp::ErrorData> {
        match self {
            Self::InvalidInput(msg) => Err(rmcp::ErrorData::invalid_params(msg, None)),
            Self::NotConfigured(msg) | Self::OperationFailed(msg) | Self::IndexNotReady(msg) => {
                Ok(CallToolResult::error(vec![Content::text(msg)]))
            }
        }
    }

    /// Shortcut for mapping a validation error to protocol-level `ErrorData`.
    ///
    /// Intended for `.map_err(ToolError::invalid_params)?` chains.
    pub fn invalid_params(msg: impl Into<String>) -> rmcp::ErrorData {
        rmcp::ErrorData::invalid_params(msg.into(), None)
    }
}
