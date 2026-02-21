use crate::indexing::wait_for_index_ready;
use crate::server::ApxServer;
use crate::tools::ToolResultExt;
use apx_core::databricks_sdk_doc::SDKSource;
use apx_core::interop::get_databricks_sdk_version;
use rmcp::model::*;
use rmcp::schemars;
use serde::Serialize;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DocsArgs {
    /// Documentation source (currently only "databricks-sdk-python" is supported)
    pub source: SDKSource,
    /// Search query (e.g., "create cluster", "list jobs", "databricks connect")
    pub query: String,
    /// Maximum number of results to return (default: 5)
    #[serde(default = "default_docs_limit")]
    pub num_results: usize,
    /// Optional project path. When provided, detects and uses the project's SDK version.
    #[serde(default)]
    pub app_path: Option<String>,
}

fn default_docs_limit() -> usize {
    5
}

impl ApxServer {
    pub async fn handle_docs(&self, args: DocsArgs) -> Result<CallToolResult, rmcp::ErrorData> {
        let ctx = &self.ctx;

        // Wait for SDK index to be ready (15 second timeout)
        let was_already_ready = match wait_for_index_ready(
            &ctx.index_state.sdk_ready,
            &ctx.index_state.sdk_indexed,
            "SDK documentation",
        )
        .await
        {
            Ok(ready) => ready,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(e)])),
        };

        // Get the SDK doc index
        let mut index_guard = ctx.sdk_doc_index.lock().await;

        let index = match index_guard.as_mut() {
            Some(idx) => idx,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "SDK documentation is not available. The index failed to bootstrap.",
                )]));
            }
        };

        // If app_path is provided, detect that project's SDK version and switch if different
        if let Some(ref app_path) = args.app_path {
            let project_dir = std::path::Path::new(app_path);
            match get_databricks_sdk_version(Some(project_dir)).await {
                Ok(Some(project_version)) => {
                    if let Err(e) = index.ensure_version(&args.source, &project_version).await {
                        tracing::warn!(
                            "Failed to switch to project SDK version {}: {}. Using current version.",
                            project_version,
                            e
                        );
                    }
                }
                Ok(None) => {
                    tracing::debug!(
                        "No SDK version detected for project at {}, using default",
                        app_path
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to detect SDK version for project at {}: {}",
                        app_path,
                        e
                    );
                }
            }
        }

        match index
            .search(&args.source, &args.query, args.num_results)
            .await
        {
            Ok(results) => {
                drop(index_guard);

                tool_response! {
                    struct DocsResponse {
                        source: String,
                        query: String,
                        results: Vec<DocsResult>,
                        #[serde(skip_serializing_if = "Option::is_none")]
                        note: Option<String>,
                    }
                }

                #[derive(Serialize)]
                struct DocsResult {
                    text: String,
                    source_file: String,
                    score: f32,
                }

                let note = if results.is_empty() && !was_already_ready {
                    Some(
                        "Index was still initializing when this query arrived. \
                         Results may be incomplete — retry in a few seconds."
                            .to_string(),
                    )
                } else {
                    None
                };

                let response = DocsResponse {
                    source: match args.source {
                        SDKSource::DatabricksSdkPython => "databricks-sdk-python".to_string(),
                    },
                    query: args.query,
                    results: results
                        .into_iter()
                        .map(|r| DocsResult {
                            text: r.text,
                            source_file: r.source_file,
                            score: r.score,
                        })
                        .collect(),
                    note,
                };

                Ok(CallToolResult::from_serializable(&response))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}
