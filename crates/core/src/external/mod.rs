//! Shared abstraction for external tool invocations (uv, bun, git, gh, databricks).
//!
//! Provides [`CommandOutput`] / [`CommandError`] value types, the [`ExternalTool`]
//! trait for resolved-binary tools, and [`run_command`] / [`run_command_sync`]
//! free functions that replace the repeated `.output().await + status-check` pattern.

pub mod bun;
pub mod databricks;
pub mod gh;
pub mod git;
pub mod uv;

use std::path::Path;

use crate::download::BinarySource;

// Re-export the per-tool types at the `external` level for ergonomic imports.
pub use bun::Bun;
pub use databricks::DatabricksCli;
pub use gh::Gh;
pub use git::Git;
pub use uv::{Uv, UvTool};

// ---------------------------------------------------------------------------
// CommandOutput
// ---------------------------------------------------------------------------

/// Captured output from an external command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl CommandOutput {
    fn from_output(output: std::process::Output) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code(),
        }
    }

    /// Return `Ok(self)` if exit code == 0, else `Err(CommandError::Failed)`.
    pub fn check(self, tool: &'static str) -> Result<Self, CommandError> {
        let code = self.exit_code.unwrap_or(-1);
        if code == 0 {
            Ok(self)
        } else {
            Err(CommandError::Failed {
                tool,
                code,
                stdout: self.stdout,
                stderr: self.stderr,
            })
        }
    }

    /// Check success and return trimmed stdout.
    pub fn into_stdout(self, tool: &'static str) -> Result<String, CommandError> {
        let checked = self.check(tool)?;
        Ok(checked.stdout.trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// CommandError
// ---------------------------------------------------------------------------

/// Unified error type for all external command failures.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{tool} not found — {hint}")]
    NotFound {
        tool: &'static str,
        hint: &'static str,
    },
    #[error("failed to spawn {tool}: {source}")]
    Spawn {
        tool: &'static str,
        source: std::io::Error,
    },
    #[error("{tool} failed (exit {code}):\n{stderr}")]
    Failed {
        tool: &'static str,
        code: i32,
        stdout: String,
        stderr: String,
    },
    #[error("{tool} timed out after {timeout_secs}s")]
    Timeout {
        tool: &'static str,
        timeout_secs: f64,
    },
}

impl CommandError {
    /// Classify an `io::Error` as `NotFound` or `Spawn`.
    pub fn from_io(tool: &'static str, hint: &'static str, err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::NotFound {
            Self::NotFound { tool, hint }
        } else {
            Self::Spawn { tool, source: err }
        }
    }
}

/// Backward-compat: many callers still use `Result<_, String>`.
impl From<CommandError> for String {
    fn from(e: CommandError) -> Self {
        e.to_string()
    }
}

// ---------------------------------------------------------------------------
// ExternalTool trait
// ---------------------------------------------------------------------------

/// Marker trait for a resolved external binary.
///
/// Provides `tokio_command()` / `std_command()` builders. Callers customise
/// args/env/cwd then pass the command to [`run_command`].
pub trait ExternalTool: std::fmt::Debug + Send + Sync {
    const NAME: &'static str;
    fn binary_path(&self) -> &Path;
    fn source(&self) -> &BinarySource;

    fn tokio_command(&self) -> tokio::process::Command {
        tokio::process::Command::new(self.binary_path())
    }

    fn std_command(&self) -> std::process::Command {
        std::process::Command::new(self.binary_path())
    }
}

// ---------------------------------------------------------------------------
// run helpers
// ---------------------------------------------------------------------------

/// Execute a tokio command, capture output, and convert to [`CommandOutput`].
///
/// Does **not** check the exit code — call `.check(tool)` or `.into_stdout(tool)`
/// on the result if you need success verification.
pub async fn run_command(
    mut cmd: tokio::process::Command,
    tool: &'static str,
) -> Result<CommandOutput, CommandError> {
    let output = cmd.output().await.map_err(|e| {
        CommandError::from_io(tool, "make sure it is installed and available in PATH", e)
    })?;
    Ok(CommandOutput::from_output(output))
}

/// Synchronous variant of [`run_command`] for blocking contexts.
pub fn run_command_sync(
    mut cmd: std::process::Command,
    tool: &'static str,
) -> Result<CommandOutput, CommandError> {
    let output = cmd.output().map_err(|e| {
        CommandError::from_io(tool, "make sure it is installed and available in PATH", e)
    })?;
    Ok(CommandOutput::from_output(output))
}
