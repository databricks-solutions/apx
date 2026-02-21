//! `uv` binary abstraction — replaces [`UvCommand`] and [`ApxCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use super::{BinarySource, ExternalTool, Resolvable, ResolvedBinary, resolve_with_download};

// ---------------------------------------------------------------------------
// Uv — resolved uv binary
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
const UV_EXE: &str = "uv.exe";
#[cfg(not(target_os = "windows"))]
const UV_EXE: &str = "uv";

const UV_VERSION: &str = "0.10.3";

static UV_CELL: OnceCell<ResolvedBinary> = OnceCell::const_new();

/// A resolved `uv` binary.
#[derive(Debug, Clone)]
pub struct Uv {
    path: PathBuf,
    source: BinarySource,
}

impl Uv {
    /// Resolve uv binary (downloads if needed). Cached after first call.
    pub async fn resolve() -> Result<Self, String> {
        let resolved = UV_CELL
            .get_or_try_init(resolve_with_download::<Self>)
            .await?;
        tracing::debug!(
            "using {} uv: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self::from_resolved(resolved.clone()))
    }

    /// Sync resolve (no download). Returns cached result if available.
    pub fn try_resolve() -> Result<Self, String> {
        if let Some(cached) = UV_CELL.get() {
            return Ok(Self::from_resolved(cached.clone()));
        }
        super::resolve_local::<Self>().map(Self::from_resolved)
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

impl Resolvable for Uv {
    const EXE_NAME: &'static str = UV_EXE;
    const ENV_VAR: Option<&'static str> = Some("APX_UV_PATH");
    const PINNED_VERSION: Option<&'static str> = Some(UV_VERSION);
    const VERSION_MARKER: Option<&'static str> = Some(".uv-version");
    const INSTALL_HINT: &'static str =
        "Install uv (https://docs.astral.sh/uv/) or set APX_UV_PATH.";

    fn from_resolved(resolved: ResolvedBinary) -> Self {
        Self {
            path: resolved.path,
            source: resolved.source,
        }
    }

    async fn download() -> Result<ResolvedBinary, String> {
        eprintln!("uv not found on PATH — downloading v{UV_VERSION}...");
        let path = crate::download::download_uv().await.map_err(|e| {
            format!(
                "Failed to auto-install uv v{UV_VERSION}: {e}\n  \
                 Install uv manually (https://docs.astral.sh/uv/) or set APX_UV_PATH."
            )
        })?;
        eprintln!("uv v{UV_VERSION} installed to {}", path.display());
        Ok(ResolvedBinary {
            path,
            source: BinarySource::ApxManaged,
        })
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
