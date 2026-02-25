use crate::server::ApxServer;
use crate::tools::{AppPathArgs, ToolError, ToolResultExt};
use crate::validation::validated_app_path;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::schemars;

/// Arguments for the `logs` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LogsToolArgs {
    /// Absolute path to the project directory
    pub app_path: String,
    /// Log duration (e.g. '5m', '1h')
    #[serde(default = "default_logs_duration")]
    pub duration: String,
}

fn default_logs_duration() -> String {
    apx_core::ops::logs::DEFAULT_LOG_DURATION.to_string()
}

impl ApxServer {
    /// Handle the `start` tool call (start dev server).
    pub async fn handle_start(&self, args: AppPathArgs) -> Result<CallToolResult, ErrorData> {
        let path = validated_app_path(&args.app_path)?;

        use apx_core::common::OutputMode;
        use apx_core::ops::dev::start_dev_server;

        match start_dev_server(&path, false, OutputMode::Quiet).await {
            Ok(port) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Dev server started at http://{}:{port}",
                apx_common::hosts::BROWSER_HOST
            ))])),
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }

    /// Handle the `stop` tool call (stop dev server).
    pub async fn handle_stop(&self, args: AppPathArgs) -> Result<CallToolResult, ErrorData> {
        let path = validated_app_path(&args.app_path)?;

        use apx_core::common::OutputMode;
        use apx_core::ops::dev::stop_dev_server;

        match stop_dev_server(&path, OutputMode::Quiet).await {
            Ok(true) => Ok(CallToolResult::success(vec![Content::text(
                "Dev server stopped",
            )])),
            Ok(false) => Ok(CallToolResult::success(vec![Content::text(
                "No dev server running",
            )])),
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }

    /// Handle the `restart` tool call (restart dev server).
    pub async fn handle_restart(&self, args: AppPathArgs) -> Result<CallToolResult, ErrorData> {
        let path = validated_app_path(&args.app_path)?;

        use apx_core::common::OutputMode;
        use apx_core::ops::dev::restart_dev_server;

        match restart_dev_server(&path, false, OutputMode::Quiet).await {
            Ok(port) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Dev server restarted at http://{}:{port}",
                apx_common::hosts::BROWSER_HOST
            ))])),
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }

    /// Handle the `logs` tool call (fetch dev server logs).
    pub async fn handle_logs(&self, args: LogsToolArgs) -> Result<CallToolResult, ErrorData> {
        let path = validated_app_path(&args.app_path)?;

        use apx_core::ops::logs::fetch_logs_structured;

        match fetch_logs_structured(&path, &args.duration).await {
            Ok(entries) => {
                tool_response! {
                    struct LogsResponse {
                        duration: String,
                        count: usize,
                        entries: Vec<apx_core::ops::logs::LogEntry>,
                    }
                }

                let response = LogsResponse {
                    duration: args.duration,
                    count: entries.len(),
                    entries,
                };

                Ok(CallToolResult::from_serializable(&response))
            }
            Err(e) => ToolError::OperationFailed(e).into_result(),
        }
    }
}
