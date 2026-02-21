//! `gh` CLI abstraction — wraps GitHub CLI operations used in `feedback.rs`.

use std::path::PathBuf;

use super::{CommandError, ToolCommand, ToolInfo, ToolInfoEntry, get_version};

/// A resolved `gh` (GitHub CLI) binary.
#[derive(Debug, Clone)]
pub struct Gh {
    path: PathBuf,
}

impl Gh {
    /// Resolve `gh` from PATH. Returns `CommandError::NotFound` if missing.
    pub fn new() -> Result<Self, CommandError> {
        let path = which::which("gh").map_err(|_| CommandError::NotFound {
            tool: "gh",
            hint: "install GitHub CLI: https://cli.github.com",
        })?;
        Ok(Self { path })
    }

    /// Create a `ToolCommand` for the gh binary.
    pub fn cmd(&self) -> ToolCommand {
        ToolCommand::new(self.path.clone(), "gh")
    }

    /// Create a GitHub issue via `gh issue create`.
    pub async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<String, CommandError> {
        let mut cmd = self.cmd().args([
            "issue", "create", "--repo", repo, "--title", title, "--body", body,
        ]);
        for label in labels {
            cmd = cmd.args(["--label", *label]);
        }
        cmd.exec_stdout().await
    }
}

impl ToolInfo for Gh {
    async fn info() -> ToolInfoEntry {
        match which::which("gh") {
            Ok(path) => {
                let version = get_version(&path).await;
                ToolInfoEntry {
                    emoji: "\u{1f419}",
                    name: "gh",
                    version: Some(version),
                    path: Some(path.display().to_string()),
                    source: None,
                    error: None,
                }
            }
            Err(_) => ToolInfoEntry {
                emoji: "\u{1f419}",
                name: "gh",
                version: None,
                path: None,
                source: None,
                error: Some(
                    "gh not found — install GitHub CLI: https://cli.github.com".to_string(),
                ),
            },
        }
    }
}
