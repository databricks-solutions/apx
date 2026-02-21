//! `gh` CLI abstraction — wraps GitHub CLI operations used in `feedback.rs`.

use std::path::PathBuf;

use super::{CommandError, run_command};

/// A resolved `gh` (GitHub CLI) binary.
#[derive(Debug, Clone)]
pub struct Gh {
    path: PathBuf,
}

impl Gh {
    /// Resolve `gh` from PATH. Returns `CommandError::NotFound` if missing.
    pub fn resolve() -> Result<Self, CommandError> {
        let path = which::which("gh").map_err(|_| CommandError::NotFound {
            tool: "gh",
            hint: "install GitHub CLI: https://cli.github.com",
        })?;
        Ok(Self { path })
    }

    /// Create a GitHub issue via `gh issue create`.
    pub async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<String, CommandError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.args([
            "issue", "create", "--repo", repo, "--title", title, "--body", body,
        ]);
        for label in labels {
            cmd.args(["--label", label]);
        }
        let output = run_command(cmd, "gh").await?.check("gh")?;
        Ok(output.stdout.trim().to_string())
    }
}
