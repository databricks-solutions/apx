//! Shared abstraction for external tool invocations (uv, bun, git, gh, databricks).
//!
//! Provides [`CommandOutput`] / [`CommandError`] value types, the [`ExternalTool`]
//! trait for resolved-binary tools, the [`Resolvable`] trait for tools that support
//! automatic resolution and optional download, and [`ToolCommand`] which wraps
//! `tokio::process::Command` with tool-name context and ergonomic terminal methods.

pub mod bun;
pub mod databricks;
pub mod gh;
pub mod git;
pub mod uv;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tracing::debug;

// Re-export the per-tool types at the `external` level for ergonomic imports.
pub use bun::Bun;
pub use databricks::DatabricksCli;
pub use gh::Gh;
pub use git::Git;
pub use uv::{Uv, UvTool};

// ---------------------------------------------------------------------------
// BinarySource / ResolvedBinary
// ---------------------------------------------------------------------------

/// Where a binary was found.
#[derive(Debug, Clone)]
pub enum BinarySource {
    EnvOverride,
    SystemPath,
    ApxManaged,
}

impl BinarySource {
    pub fn source_label(&self) -> &'static str {
        match self {
            BinarySource::EnvOverride => "env-override",
            BinarySource::SystemPath => "system",
            BinarySource::ApxManaged => "apx-provided",
        }
    }
}

/// A resolved binary path with its source.
#[derive(Debug, Clone)]
pub struct ResolvedBinary {
    pub path: PathBuf,
    pub source: BinarySource,
}

impl ResolvedBinary {
    pub fn source_label(&self) -> &'static str {
        self.source.source_label()
    }
}

// ---------------------------------------------------------------------------
// ToolCommand — fluent builder for external tool invocations
// ---------------------------------------------------------------------------

/// Fluent builder for constructing and executing external tool commands.
///
/// Wraps `tokio::process::Command` with tool-name context for error messages.
/// Callers obtain a `ToolCommand` via `<Tool>::cmd()` and chain `.arg()`,
/// `.args()`, `.env()`, `.cwd()` before finishing with `.exec()`,
/// `.exec_checked()`, `.exec_stdout()`, `.spawn()`, or `.into_command()`.
#[derive(Debug)]
pub struct ToolCommand {
    inner: tokio::process::Command,
    tool_name: &'static str,
}

impl ToolCommand {
    pub(crate) fn new(binary: PathBuf, tool_name: &'static str) -> Self {
        Self {
            inner: tokio::process::Command::new(binary),
            tool_name,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.inner.arg(arg);
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        self.inner.args(args);
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Self {
        self.inner.env(key, val);
        self
    }

    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl AsRef<OsStr>, impl AsRef<OsStr>)>,
    ) -> Self {
        self.inner.envs(vars);
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.inner.current_dir(dir.into());
        self
    }

    pub fn stdin(mut self, cfg: Stdio) -> Self {
        self.inner.stdin(cfg);
        self
    }

    pub fn stdout(mut self, cfg: Stdio) -> Self {
        self.inner.stdout(cfg);
        self
    }

    pub fn stderr(mut self, cfg: Stdio) -> Self {
        self.inner.stderr(cfg);
        self
    }

    /// Convert to a raw `tokio::process::Command` for streaming / custom handling.
    pub fn into_command(self) -> tokio::process::Command {
        self.inner
    }

    /// Run and capture output (does NOT check exit code).
    pub async fn exec(mut self) -> Result<CommandOutput, CommandError> {
        let tool = self.tool_name;
        let output = self.inner.output().await.map_err(|e| {
            CommandError::from_io(tool, "make sure it is installed and available in PATH", e)
        })?;
        Ok(CommandOutput::from_output(output))
    }

    /// Run, check exit code == 0.
    pub async fn exec_checked(self) -> Result<CommandOutput, CommandError> {
        let tool = self.tool_name;
        self.exec().await?.check(tool)
    }

    /// Run, check exit code, return trimmed stdout.
    pub async fn exec_stdout(self) -> Result<String, CommandError> {
        let tool = self.tool_name;
        self.exec().await?.into_stdout(tool)
    }

    /// Spawn without waiting (for long-running processes).
    pub fn spawn(mut self) -> Result<tokio::process::Child, CommandError> {
        let tool = self.tool_name;
        self.inner.spawn().map_err(|e| {
            CommandError::from_io(tool, "make sure it is installed and available in PATH", e)
        })
    }
}

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
/// Provides identity (name, path, source) for a resolved tool. Concrete types
/// expose `cmd() -> ToolCommand` and public domain methods instead.
pub trait ExternalTool: std::fmt::Debug + Send + Sync {
    const NAME: &'static str;
    fn binary_path(&self) -> &Path;
    fn source(&self) -> &BinarySource;
}

// ---------------------------------------------------------------------------
// Resolvable trait
// ---------------------------------------------------------------------------

/// Trait for external tools that support resolution with optional auto-download.
///
/// Resolution order (implemented by [`resolve_local`]):
/// 1. Environment variable override (`ENV_VAR`)
/// 2. System PATH via `which::which()`
/// 3. `~/.apx/bin/` with version marker (only when `PINNED_VERSION` is set)
/// 4. Auto-download via [`Resolvable::download`] (only when implemented)
pub trait Resolvable: ExternalTool + Sized {
    /// Platform-specific executable filename (e.g. `"bun"` or `"bun.exe"` on Windows).
    const EXE_NAME: &'static str;

    /// Environment variable for explicit path override (e.g. `"APX_BUN_PATH"`).
    /// `None` for tools that don't support env override.
    const ENV_VAR: Option<&'static str>;

    /// Pinned version for managed installs. `None` for tools not auto-downloaded.
    const PINNED_VERSION: Option<&'static str>;

    /// Version marker filename in `~/.apx/bin/` (e.g. `".bun-version"`).
    /// `None` for tools not auto-downloaded.
    const VERSION_MARKER: Option<&'static str>;

    /// Human-readable install hint shown when the tool cannot be found.
    const INSTALL_HINT: &'static str;

    /// Construct `Self` from a resolved binary.
    fn from_resolved(resolved: ResolvedBinary) -> Self;

    /// Auto-download and install the tool. Returns the resolved binary on success.
    ///
    /// Default implementation returns an error (for tools that are not auto-downloaded).
    fn download() -> impl std::future::Future<Output = Result<ResolvedBinary, String>> + Send {
        async {
            Err(format!(
                "Cannot auto-download {}. {}",
                Self::NAME,
                Self::INSTALL_HINT
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Generic resolution functions
// ---------------------------------------------------------------------------

/// Try to resolve a [`Resolvable`] tool locally (env var → PATH → `~/.apx/bin/`).
///
/// Does **not** download. Use [`resolve_with_download`] for the full resolution flow.
pub fn resolve_local<T: Resolvable>() -> Result<ResolvedBinary, String> {
    // 1. Env var override
    if let Some(env_var) = T::ENV_VAR
        && let Ok(path) = std::env::var(env_var)
    {
        let p = PathBuf::from(&path);
        if p.is_file() {
            debug!("{env_var}={} — using env override", p.display());
            return Ok(ResolvedBinary {
                path: p,
                source: BinarySource::EnvOverride,
            });
        }
        return Err(format!("{env_var}={path} does not exist"));
    }

    // 2. System PATH
    if let Ok(path) = which::which(T::EXE_NAME) {
        debug!("{} found on PATH at {}", T::EXE_NAME, path.display());
        return Ok(ResolvedBinary {
            path,
            source: BinarySource::SystemPath,
        });
    }

    // 3. ~/.apx/bin/ with version marker
    if let (Some(version), Some(marker)) = (T::PINNED_VERSION, T::VERSION_MARKER)
        && let Some(bin_dir) = crate::download::apx_bin_dir()
    {
        let candidate = bin_dir.join(T::EXE_NAME);
        let marker_path = bin_dir.join(marker);
        if candidate.is_file()
            && let Ok(contents) = std::fs::read_to_string(&marker_path)
        {
            if contents.trim() == version {
                debug!(
                    "{} found in ~/.apx/bin/ (v{version}): {}",
                    T::EXE_NAME,
                    candidate.display()
                );
                return Ok(ResolvedBinary {
                    path: candidate,
                    source: BinarySource::ApxManaged,
                });
            }
            debug!(
                "{} in ~/.apx/bin/ has version '{}', need '{version}' — will re-download",
                T::EXE_NAME,
                contents.trim()
            );
        }
    }

    Err(format!("Could not find {}. {}", T::NAME, T::INSTALL_HINT))
}

/// Resolve a [`Resolvable`] tool: try local, then download as fallback.
pub async fn resolve_with_download<T: Resolvable>() -> Result<ResolvedBinary, String> {
    if let Ok(resolved) = resolve_local::<T>() {
        return Ok(resolved);
    }
    T::download().await
}

// ---------------------------------------------------------------------------
// ToolInfo trait — for `apx info` display
// ---------------------------------------------------------------------------

/// A single entry for `apx info` output.
#[derive(Debug)]
pub struct ToolInfoEntry {
    pub emoji: &'static str,
    pub name: &'static str,
    pub version: Option<String>,
    pub path: Option<String>,
    pub source: Option<String>,
    pub error: Option<String>,
}

/// Trait for tools that can report their info for `apx info`.
pub trait ToolInfo {
    fn info() -> impl std::future::Future<Output = ToolInfoEntry> + Send;
}

/// Run `<binary> --version` and return the trimmed stdout, or `"unknown"`.
pub(crate) async fn get_version(path: &Path) -> String {
    tokio::process::Command::new(path)
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
