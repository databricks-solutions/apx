//! `uv` binary abstraction — replaces [`UvCommand`] and [`ApxCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use crate::download::{BinarySource, ResolvedBinary, resolve_uv, try_resolve_uv};

use super::ExternalTool;

// ---------------------------------------------------------------------------
// Uv — resolved uv binary
// ---------------------------------------------------------------------------

/// A resolved `uv` binary.
#[derive(Debug, Clone)]
pub struct Uv {
    path: PathBuf,
    source: BinarySource,
}

impl Uv {
    /// Resolve uv binary (downloads if needed).
    pub async fn resolve() -> Result<Self, String> {
        let resolved = resolve_uv().await?;
        tracing::debug!(
            "using {} uv: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self::from_resolved(resolved))
    }

    /// Sync resolve (no download).
    pub fn try_resolve() -> Result<Self, String> {
        let resolved = try_resolve_uv()?;
        Ok(Self::from_resolved(resolved))
    }

    fn from_resolved(resolved: ResolvedBinary) -> Self {
        Self {
            path: resolved.path,
            source: resolved.source,
        }
    }
}

impl ExternalTool for Uv {
    const NAME: &'static str = "uv";

    fn binary_path(&self) -> &Path {
        &self.path
    }

    fn source(&self) -> &BinarySource {
        &self.source
    }
}

// ---------------------------------------------------------------------------
// UvTool — `uv run <tool>` wrapper
// ---------------------------------------------------------------------------

/// Wraps [`Uv`] to invoke a specific tool via `uv run <tool>`.
#[derive(Debug, Clone)]
pub struct UvTool {
    uv: Uv,
    tool: &'static str,
}

impl UvTool {
    /// Resolve uv and create a `UvTool` for the specified tool name.
    pub async fn resolve(tool: &'static str) -> Result<Self, String> {
        Ok(Self {
            uv: Uv::resolve().await?,
            tool,
        })
    }

    /// Sync resolve (no download).
    pub fn try_resolve(tool: &'static str) -> Result<Self, String> {
        Ok(Self {
            uv: Uv::try_resolve()?,
            tool,
        })
    }

    /// The underlying `Uv`.
    pub fn uv(&self) -> &Uv {
        &self.uv
    }

    /// The tool name (e.g. `"apx"`, `"uvicorn"`, `"ty"`).
    pub fn tool_name(&self) -> &'static str {
        self.tool
    }

    /// Build a `tokio::process::Command` that runs `uv run <tool>`.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.uv.path);
        cmd.args(["run", self.tool]);
        cmd
    }

    /// Build a `std::process::Command` that runs `uv run <tool>`.
    pub fn std_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.uv.path);
        cmd.args(["run", self.tool]);
        cmd
    }

    /// Format the command for display/logging.
    pub fn display(&self) -> String {
        format!("uv run {}", self.tool)
    }
}

/// Type alias for `UvTool` configured to run `apx`.
pub type ApxTool = UvTool;

impl ApxTool {
    /// Resolve uv and create an `ApxTool` (i.e. `uv run apx`).
    pub async fn resolve_apx() -> Result<Self, String> {
        Self::resolve("apx").await
    }
}
