use crate::indexing::wait_for_index_ready;
use crate::server::ApxServer;
use crate::tools::ToolResultExt;
use apx_core::databricks_sdk_doc::SDKSource;
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
}

fn default_docs_limit() -> usize {
    5
}

impl ApxServer {
    pub async fn handle_docs(
        &self,
        args: DocsArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let ctx = &self.ctx;

        // Wait for SDK index to be ready (15 second timeout)
        if let Err(e) = wait_for_index_ready(
            &ctx.index_state.sdk_ready,
            &ctx.index_state.sdk_indexed,
            "SDK documentation",
        )
        .await
        {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }

        // Get the SDK doc index
        let index_guard = ctx.sdk_doc_index.lock().await;

        let index = match index_guard.as_ref() {
            Some(idx) => idx,
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "SDK documentation is not available. The Databricks SDK may not be installed or the index failed to bootstrap."
                )]));
            }
        };

        match index.search_sync(&args.source, &args.query, args.num_results) {
            Ok(results) => {
                drop(index_guard);

                #[derive(Serialize)]
                struct DocsResponse {
                    source: String,
                    query: String,
                    results: Vec<DocsResult>,
                }

                #[derive(Serialize)]
                struct DocsResult {
                    text: String,
                    source_file: String,
                    score: f32,
                }

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
                };

                Ok(CallToolResult::from_serializable(&response))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}
