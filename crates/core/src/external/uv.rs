//! `uv` binary abstraction — replaces [`UvCommand`] and [`ApxCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use super::{
    BinarySource, CommandError, CommandOutput, ExternalTool, Resolvable, ResolvedBinary,
    ToolCommand, ToolInfo, ToolInfoEntry, get_version, resolve_local, resolve_with_download,
};

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
    pub async fn new() -> Result<Self, String> {
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
    pub fn try_new() -> Result<Self, String> {
        if let Some(cached) = UV_CELL.get() {
            return Ok(Self::from_resolved(cached.clone()));
        }
        super::resolve_local::<Self>().map(Self::from_resolved)
    }

    /// Create a `ToolCommand` for the uv binary.
    pub fn cmd(&self) -> ToolCommand {
        ToolCommand::new(self.path.clone(), "uv")
    }

    // -----------------------------------------------------------------------
    // Domain methods
    // -----------------------------------------------------------------------

    /// Run `uv sync` in the given directory.
    pub async fn sync(&self, cwd: &Path) -> Result<CommandOutput, CommandError> {
        self.cmd().arg("sync").cwd(cwd).exec().await
    }

    /// Run `uv tool run <tool>` in the given directory.
    pub async fn tool_run(&self, cwd: &Path, tool: &str) -> Result<CommandOutput, CommandError> {
        self.cmd().args(["tool", "run", tool]).cwd(cwd).exec().await
    }

    /// Run `uv run --no-sync python -c <code> [script_args]` in the given directory.
    pub async fn run_python_code(
        &self,
        cwd: &Path,
        code: &str,
        script_args: &[&str],
    ) -> Result<CommandOutput, CommandError> {
        self.cmd()
            .args(["run", "--no-sync", "python", "-c", code])
            .args(script_args)
            .cwd(cwd)
            .exec()
            .await
    }

    /// Build a `ToolCommand` for `uv build --wheel --out-dir <out_dir>`.
    /// Returns the command for the caller to configure streaming output via `.into_command()`.
    pub fn build_wheel_command(&self, cwd: &Path, out_dir: &Path) -> ToolCommand {
        self.cmd()
            .arg("build")
            .arg("--wheel")
            .arg("--out-dir")
            .arg(out_dir)
            .cwd(cwd)
    }

    /// Run `uv run hatch version` and return the version string.
    pub async fn run_hatch_version(&self, cwd: &Path) -> Result<String, CommandError> {
        self.cmd()
            .args(["run", "hatch", "version"])
            .cwd(cwd)
            .exec_stdout()
            .await
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

impl ToolInfo for Uv {
    async fn info() -> ToolInfoEntry {
        match resolve_local::<Self>() {
            Ok(resolved) => {
                let version = get_version(&resolved.path).await;
                ToolInfoEntry {
                    emoji: "\u{1f40d}",
                    name: "uv",
                    version: Some(version),
                    path: Some(resolved.path.display().to_string()),
                    source: Some(resolved.source.source_label().to_string()),
                    error: None,
                }
            }
            Err(e) => ToolInfoEntry {
                emoji: "\u{1f40d}",
                name: "uv",
                version: None,
                path: None,
                source: None,
                error: Some(e),
            },
        }
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
    pub async fn new(tool: &'static str) -> Result<Self, String> {
        Ok(Self {
            uv: Uv::new().await?,
            tool,
        })
    }

    /// Sync resolve (no download).
    pub fn try_new(tool: &'static str) -> Result<Self, String> {
        Ok(Self {
            uv: Uv::try_new()?,
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

    /// Create a `ToolCommand` that runs `uv run <tool>`.
    pub fn cmd(&self) -> ToolCommand {
        ToolCommand::new(self.uv.path.clone(), self.tool).args(["run", self.tool])
    }

    /// Format the command for display/logging.
    pub fn display(&self) -> String {
        format!("uv run {}", self.tool)
    }

    // -----------------------------------------------------------------------
    // Domain methods
    // -----------------------------------------------------------------------

    /// Run `uv run <tool> <args>` in the given directory.
    pub async fn run(&self, cwd: &Path, args: &[&str]) -> Result<CommandOutput, CommandError> {
        self.cmd().args(args).cwd(cwd).exec().await
    }
}

/// Type alias for `UvTool` configured to run `apx`.
pub type ApxTool = UvTool;

impl ApxTool {
    /// Resolve uv and create an `ApxTool` (i.e. `uv run apx`).
    pub async fn new_apx() -> Result<Self, String> {
        Self::new("apx").await
    }
}
