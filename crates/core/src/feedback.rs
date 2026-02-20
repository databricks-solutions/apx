use std::fmt;

const GITHUB_REPO: &str = "databricks-solutions/apx";

/// Metadata auto-collected for feedback issues.
#[derive(Debug, Clone)]
pub struct FeedbackMetadata {
    pub apx_version: String,
    pub os: String,
    pub arch: String,
}

/// Prepared feedback ready for preview and submission.
#[derive(Debug, Clone)]
pub struct PreparedFeedback {
    pub title: String,
    pub body: String,
    pub browser_url: String,
}

/// Result of a feedback submission attempt.
#[derive(Debug)]
pub enum FeedbackResult {
    /// Successfully created a GitHub issue.
    Submitted { url: String },
    /// Could not submit automatically; includes fallback info.
    Fallback {
        title: String,
        body: String,
        url: String,
    },
}

/// Errors from `gh` CLI submission.
#[derive(Debug)]
pub enum FeedbackError {
    GhNotFound,
    GhFailed(String),
}

impl fmt::Display for FeedbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GhNotFound => write!(f, "gh CLI not found"),
            Self::GhFailed(msg) => write!(f, "gh CLI error: {msg}"),
        }
    }
}

/// Collect metadata about the current environment.
pub fn collect_metadata() -> FeedbackMetadata {
    FeedbackMetadata {
        apx_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

/// Format the issue title.
pub fn format_issue_title(title: Option<&str>, message: &str) -> String {
    let suffix = match title {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            let trimmed = message.trim();
            // Take first line, truncated to ~50 chars
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            if first_line.len() > 50 {
                format!("{}...", &first_line[..50])
            } else {
                first_line.to_string()
            }
        }
    };
    format!("\u{1f4ac} [FEEDBACK] {suffix}")
}

/// Format the issue body with message, optional category, and optional metadata.
pub fn format_issue_body(
    message: &str,
    category: Option<&str>,
    metadata: Option<&FeedbackMetadata>,
) -> String {
    let mut body = String::new();

    if let Some(cat) = category
        && !cat.is_empty()
    {
        body.push_str(&format!("**Category**: {cat}\n\n"));
    }

    body.push_str(message);

    if let Some(meta) = metadata {
        body.push_str("\n\n---\n");
        body.push_str("**Metadata** (auto-collected)\n");
        body.push_str(&format!("- apx version: {}\n", meta.apx_version));
        body.push_str(&format!("- OS: {}\n", meta.os));
        body.push_str(&format!("- Arch: {}\n", meta.arch));
    }

    body
}

/// Submit feedback via the `gh` CLI. Returns the issue URL on success.
pub async fn submit_via_gh_cli(title: &str, body: &str) -> Result<String, FeedbackError> {
    // Check if gh is available
    if which::which("gh").is_err() {
        return Err(FeedbackError::GhNotFound);
    }

    let output = tokio::process::Command::new("gh")
        .args([
            "issue",
            "create",
            "--repo",
            GITHUB_REPO,
            "--title",
            title,
            "--body",
            body,
            "--label",
            "feedback",
        ])
        .output()
        .await
        .map_err(|e| FeedbackError::GhFailed(e.to_string()))?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(FeedbackError::GhFailed(stderr))
    }
}

/// Build a pre-filled GitHub issue URL for browser fallback.
pub fn github_new_issue_url(title: &str, body: &str) -> String {
    fn percent_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 2);
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => {
                    out.push('%');
                    out.push_str(&format!("{b:02X}"));
                }
            }
        }
        out
    }

    format!(
        "https://github.com/{GITHUB_REPO}/issues/new?title={}&body={}",
        percent_encode(title),
        percent_encode(body),
    )
}

/// Prepare feedback for preview without submitting. Returns formatted title, body, and browser URL.
pub fn prepare_feedback(
    title: Option<&str>,
    message: &str,
    category: Option<&str>,
    include_metadata: bool,
) -> PreparedFeedback {
    let metadata = if include_metadata {
        Some(collect_metadata())
    } else {
        None
    };
    let issue_title = format_issue_title(title, message);
    let issue_body = format_issue_body(message, category, metadata.as_ref());
    let browser_url = github_new_issue_url(&issue_title, &issue_body);

    PreparedFeedback {
        title: issue_title,
        body: issue_body,
        browser_url,
    }
}

/// Submit already-prepared feedback via `gh` CLI.
pub async fn submit_prepared(prepared: &PreparedFeedback) -> FeedbackResult {
    match submit_via_gh_cli(&prepared.title, &prepared.body).await {
        Ok(url) => FeedbackResult::Submitted { url },
        Err(_) => FeedbackResult::Fallback {
            title: prepared.title.clone(),
            body: prepared.body.clone(),
            url: prepared.browser_url.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_issue_title_with_explicit_title() {
        let title = format_issue_title(Some("My Title"), "some message");
        assert_eq!(title, "\u{1f4ac} [FEEDBACK] My Title");
    }

    #[test]
    fn test_format_issue_title_from_message() {
        let title = format_issue_title(None, "Short message");
        assert_eq!(title, "\u{1f4ac} [FEEDBACK] Short message");
    }

    #[test]
    fn test_format_issue_title_truncates_long_message() {
        let long = "a".repeat(100);
        let title = format_issue_title(None, &long);
        assert!(title.ends_with("..."));
        assert!(title.len() < 80);
    }

    #[test]
    fn test_format_issue_body_with_category_and_metadata() {
        let meta = FeedbackMetadata {
            apx_version: "0.3.0".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        };
        let body = format_issue_body("Great tool!", Some("feature"), Some(&meta));
        assert!(body.contains("**Category**: feature"));
        assert!(body.contains("Great tool!"));
        assert!(body.contains("apx version: 0.3.0"));
        assert!(body.contains("OS: macos"));
        assert!(body.contains("Arch: aarch64"));
    }

    #[test]
    fn test_format_issue_body_without_metadata() {
        let body = format_issue_body("Great tool!", Some("feature"), None);
        assert!(body.contains("**Category**: feature"));
        assert!(body.contains("Great tool!"));
        assert!(!body.contains("Metadata"));
    }

    #[test]
    fn test_format_issue_body_without_category() {
        let meta = FeedbackMetadata {
            apx_version: "0.3.0".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        };
        let body = format_issue_body("Bug report", None, Some(&meta));
        assert!(!body.contains("**Category**"));
        assert!(body.contains("Bug report"));
    }

    #[test]
    fn test_github_new_issue_url() {
        let url = github_new_issue_url("Test Title", "Test body");
        assert!(url.starts_with("https://github.com/databricks-solutions/apx/issues/new?"));
        assert!(url.contains("title="));
        assert!(url.contains("body="));
    }

    #[test]
    fn test_collect_metadata() {
        let meta = collect_metadata();
        assert!(!meta.apx_version.is_empty());
        assert!(!meta.os.is_empty());
        assert!(!meta.arch.is_empty());
    }

    #[test]
    fn test_prepare_feedback_with_metadata() {
        let prepared = prepare_feedback(Some("Test"), "msg", None, true);
        assert!(prepared.title.contains("[FEEDBACK] Test"));
        assert!(prepared.body.contains("Metadata"));
        assert!(prepared.browser_url.contains("github.com"));
    }

    #[test]
    fn test_prepare_feedback_without_metadata() {
        let prepared = prepare_feedback(Some("Test"), "msg", None, false);
        assert!(prepared.title.contains("[FEEDBACK] Test"));
        assert!(!prepared.body.contains("Metadata"));
    }
}
