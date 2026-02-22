use crate::server::ApxServer;
use crate::tools::{ToolError, ToolResultExt};
use rmcp::model::*;
use rmcp::schemars;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FeedbackPrepareArgs {
    /// The feedback message
    pub message: String,
    /// Optional title for the feedback issue
    #[serde(default)]
    pub title: Option<String>,
    /// Category: docs, bug, feature, skill, general
    #[serde(default = "default_category")]
    pub category: String,
    /// Include auto-collected metadata (version, OS, arch). Defaults to true.
    #[serde(default = "default_true")]
    pub include_metadata: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FeedbackSubmitArgs {
    /// The exact issue title (from feedback_prepare response)
    pub title: String,
    /// The exact issue body (from feedback_prepare response)
    pub body: String,
}

fn default_category() -> String {
    "general".to_string()
}

fn default_true() -> bool {
    true
}

impl ApxServer {
    pub async fn handle_feedback_prepare(
        &self,
        args: FeedbackPrepareArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if args.message.trim().is_empty() {
            return ToolError::InvalidInput("Feedback message cannot be empty".to_string())
                .into_result();
        }

        let category = if args.category.is_empty() {
            None
        } else {
            Some(args.category.as_str())
        };

        let prepared = apx_core::feedback::prepare_feedback(
            args.title.as_deref(),
            &args.message,
            category,
            args.include_metadata,
        );

        tool_response! {
            struct PrepareResponse {
                title: String,
                body: String,
                browser_url: String,
                note: &'static str,
            }
        }

        Ok(CallToolResult::from_serializable(&PrepareResponse {
            title: prepared.title,
            body: prepared.body,
            browser_url: prepared.browser_url,
            note: "Review the title and body above. Call feedback_submit with these values to create a public GitHub issue.",
        }))
    }

    pub async fn handle_feedback_submit(
        &self,
        args: FeedbackSubmitArgs,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if args.title.trim().is_empty() || args.body.trim().is_empty() {
            return ToolError::InvalidInput(
                "Title and body are required. Call feedback_prepare first.".to_string(),
            )
            .into_result();
        }

        let prepared = apx_core::feedback::PreparedFeedback {
            title: args.title,
            body: args.body.clone(),
            browser_url: apx_core::feedback::github_new_issue_url(&args.body, &args.body),
        };

        let result = apx_core::feedback::submit_prepared(&prepared).await;

        match result {
            apx_core::feedback::FeedbackResult::Submitted { url } => {
                tool_response! {
                    struct SubmittedResponse {
                        status: &'static str,
                        issue_url: String,
                    }
                }
                Ok(CallToolResult::from_serializable(&SubmittedResponse {
                    status: "submitted",
                    issue_url: url,
                }))
            }
            apx_core::feedback::FeedbackResult::Fallback { url, .. } => {
                tool_response! {
                    struct FallbackResponse {
                        status: &'static str,
                        message: &'static str,
                        browser_url: String,
                    }
                }
                Ok(CallToolResult::from_serializable(&FallbackResponse {
                    status: "fallback",
                    message: "Could not submit automatically (gh CLI not found or not authenticated). Share the browser_url with the user to submit manually.",
                    browser_url: url,
                }))
            }
        }
    }
}
