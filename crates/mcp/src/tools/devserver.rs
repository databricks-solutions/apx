use crate::server::ApxServer;
use crate::tools::{AppPathArgs, ToolResultExt};
use crate::validation::validate_app_path;
use rmcp::model::*;
use rmcp::schemars;
use serde::Serialize;

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
    pub async fn handle_start(&self, args: AppPathArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::OutputMode;
        use apx_core::dev::common::CLIENT_HOST;
        use apx_core::ops::dev::start_dev_server;

        match start_dev_server(&path, OutputMode::Quiet).await {
            Ok(port) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Dev server started at http://{CLIENT_HOST}:{port}"
            ))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    pub async fn handle_stop(&self, args: AppPathArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::OutputMode;
        use apx_core::ops::dev::stop_dev_server;

        match stop_dev_server(&path, OutputMode::Quiet).await {
            Ok(true) => Ok(CallToolResult::success(vec![Content::text(
                "Dev server stopped",
            )])),
            Ok(false) => Ok(CallToolResult::success(vec![Content::text(
                "No dev server running",
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    pub async fn handle_restart(
        &self,
        args: AppPathArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::common::OutputMode;
        use apx_core::ops::dev::restart_dev_server;

        match restart_dev_server(&path, OutputMode::Quiet).await {
            Ok(port) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Dev server restarted at http://localhost:{port}"
            ))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    pub async fn handle_logs(&self, args: LogsToolArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let path = validate_app_path(&args.app_path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;

        use apx_core::ops::logs::fetch_logs_structured;

        match fetch_logs_structured(&path, &args.duration).await {
            Ok(entries) => {
                #[derive(Serialize)]
                struct LogsResponse {
                    duration: String,
                    count: usize,
                    entries: Vec<apx_core::ops::logs::LogEntry>,
                }

                let response = LogsResponse {
                    duration: args.duration,
                    count: entries.len(),
                    entries,
                };

                Ok(CallToolResult::from_serializable(&response))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}
