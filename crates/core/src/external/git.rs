//! `git` binary abstraction — wraps common git operations used in `init.rs`.

use std::path::{Path, PathBuf};

use super::{CommandError, CommandOutput, ToolInfo, ToolInfoEntry, get_version, run_command};

/// A resolved `git` binary.
#[derive(Debug, Clone)]
pub struct Git {
    path: PathBuf,
}

impl Git {
    /// Check whether git is available on PATH.
    pub async fn is_available() -> bool {
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("--version");
        cmd.output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Resolve git from PATH. Returns an error if git is not installed.
    pub fn resolve() -> Result<Self, CommandError> {
        let path = which::which("git").map_err(|_| CommandError::NotFound {
            tool: "git",
            hint: "install git and make sure it is available in PATH",
        })?;
        Ok(Self { path })
    }

    /// `git init` in the given directory.
    pub async fn init(&self, dir: &Path) -> Result<CommandOutput, CommandError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.arg("init").current_dir(dir);
        run_command(cmd, "git").await?.check("git")
    }

    /// `git add <paths>` in the given directory.
    pub async fn add(&self, dir: &Path, paths: &[&str]) -> Result<CommandOutput, CommandError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.arg("add").args(paths).current_dir(dir);
        run_command(cmd, "git").await?.check("git")
    }

    /// `git commit -m <message>` in the given directory.
    pub async fn commit(&self, dir: &Path, message: &str) -> Result<CommandOutput, CommandError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.args(["commit", "-m", message]).current_dir(dir);
        run_command(cmd, "git").await?.check("git")
    }

    /// Check if `dir` is inside a git work tree.
    pub async fn is_inside_work_tree(&self, dir: &Path) -> Result<bool, CommandError> {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(dir);
        let output = run_command(cmd, "git").await?;
        Ok(output.exit_code == Some(0) && output.stdout.trim() == "true")
    }
}

impl ToolInfo for Git {
    async fn info() -> ToolInfoEntry {
        match which::which("git") {
            Ok(path) => {
                let version = get_version(&path).await;
                ToolInfoEntry {
                    emoji: "\u{1f500}",
                    name: "git",
                    version: Some(version),
                    path: Some(path.display().to_string()),
                    source: None,
                    error: None,
                }
            }
            Err(_) => ToolInfoEntry {
                emoji: "\u{1f500}",
                name: "git",
                version: None,
                path: None,
                source: None,
                error: Some(
                    "git not found — install git and make sure it is available in PATH".to_string(),
                ),
            },
        }
    }
}
