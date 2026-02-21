//! `bun` binary abstraction — replaces [`BunCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use super::{
    BinarySource, CommandError, CommandOutput, ExternalTool, Resolvable, ResolvedBinary,
    ToolCommand, ToolInfo, ToolInfoEntry, get_version, resolve_local, resolve_with_download,
};

#[cfg(target_os = "windows")]
const BUN_EXE: &str = "bun.exe";
#[cfg(not(target_os = "windows"))]
const BUN_EXE: &str = "bun";

const BUN_VERSION: &str = "1.3.8";

static BUN_CELL: OnceCell<ResolvedBinary> = OnceCell::const_new();

/// A resolved `bun` binary.
#[derive(Debug, Clone)]
pub struct Bun {
    path: PathBuf,
    source: BinarySource,
}

impl Bun {
    /// Resolve bun binary (downloads if needed). Cached after first call.
    pub async fn new() -> Result<Self, String> {
        let resolved = BUN_CELL
            .get_or_try_init(resolve_with_download::<Self>)
            .await?;
        tracing::debug!(
            "using {} bun: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self::from_resolved(resolved.clone()))
    }

    /// Build a PATH with the apx bin directory prepended.
    /// This ensures child processes spawned by bun also use the apx-bundled bun.
    fn patched_path(&self) -> std::ffi::OsString {
        let apx_bin_dir = self.path.parent().unwrap_or(Path::new(""));
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![apx_bin_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&current_path));
        std::env::join_paths(paths).unwrap_or(current_path)
    }

    /// Create a `ToolCommand` for bun (with patched PATH).
    pub fn cmd(&self) -> ToolCommand {
        ToolCommand::new(self.path.clone(), "bun").env("PATH", self.patched_path())
    }

    /// Create a `ToolCommand` with `NODE_PATH` set to `<app_dir>/node_modules`.
    ///
    /// Use this when running scripts that live outside the project directory
    /// (e.g. the bundled entrypoint.ts at ~/.apx/files/). Without NODE_PATH,
    /// bun resolves transitive dependencies relative to the script's location
    /// or its global cache, which fails to find packages installed in the
    /// project's node_modules.
    pub fn cmd_with_node_path(&self, app_dir: &Path) -> ToolCommand {
        self.cmd().env("NODE_PATH", app_dir.join("node_modules"))
    }

    // -----------------------------------------------------------------------
    // Domain methods
    // -----------------------------------------------------------------------

    /// Run `bun install` in the given directory.
    pub async fn install(&self, cwd: &Path) -> Result<CommandOutput, CommandError> {
        let mut cmd = self.cmd().arg("install");
        if let Ok(cache_dir) = std::env::var("BUN_CACHE_DIR") {
            cmd = cmd.arg("--cache-dir").arg(cache_dir);
        }
        cmd.cwd(cwd).exec().await
    }

    /// Run `bun add <deps>` in the given directory.
    pub async fn add(&self, cwd: &Path, deps: &[String]) -> Result<CommandOutput, CommandError> {
        self.cmd().arg("add").args(deps).cwd(cwd).exec().await
    }

    /// Run `bun add --dev <deps>` in the given directory.
    pub async fn add_dev(&self, cwd: &Path, deps: &[&str]) -> Result<CommandOutput, CommandError> {
        self.cmd()
            .arg("add")
            .arg("--dev")
            .args(deps)
            .cwd(cwd)
            .exec()
            .await
    }

    /// Run `bun run <script> [args]` in the given directory.
    pub async fn run_script(
        &self,
        cwd: &Path,
        script: &str,
        args: &[&str],
    ) -> Result<CommandOutput, CommandError> {
        self.cmd()
            .arg("run")
            .arg(script)
            .args(args)
            .cwd(cwd)
            .exec()
            .await
    }

    /// Run `bun run <entrypoint> [args]` with `NODE_PATH` and `APX_APP_NAME`.
    pub async fn run_entrypoint(
        &self,
        app_dir: &Path,
        entrypoint: &Path,
        args: &[String],
        app_name: &str,
    ) -> Result<CommandOutput, CommandError> {
        self.cmd_with_node_path(app_dir)
            .arg("run")
            .arg(entrypoint)
            .args(args)
            .env("APX_APP_NAME", app_name)
            .cwd(app_dir)
            .exec()
            .await
    }

    /// Spawn `bun run <entrypoint> [args]` with `NODE_PATH` and `APX_APP_NAME`.
    /// Returns the child process for the caller to manage.
    pub fn spawn_entrypoint(
        &self,
        app_dir: &Path,
        entrypoint: &Path,
        args: &[String],
        app_name: &str,
    ) -> Result<tokio::process::Child, CommandError> {
        self.cmd_with_node_path(app_dir)
            .arg("run")
            .arg(entrypoint)
            .args(args)
            .env("APX_APP_NAME", app_name)
            .cwd(app_dir)
            .spawn()
    }

    /// Spawn `bun <args>` for passthrough execution.
    /// Returns the child process for the caller to manage.
    pub fn passthrough(&self, args: &[String]) -> Result<tokio::process::Child, CommandError> {
        self.cmd().args(args).spawn()
    }

    /// Build a `ToolCommand` for `bun run <entrypoint>` with `NODE_PATH` and `APX_APP_NAME`.
    /// Returns the command for the caller to configure streaming output via `.into_command()`.
    pub fn entrypoint_command(
        &self,
        app_dir: &Path,
        entrypoint: &Path,
        args: &[String],
        app_name: &str,
    ) -> ToolCommand {
        self.cmd_with_node_path(app_dir)
            .arg("run")
            .arg(entrypoint)
            .args(args)
            .env("APX_APP_NAME", app_name)
            .cwd(app_dir)
    }
}

impl ExternalTool for Bun {
    const NAME: &'static str = "bun";

    fn binary_path(&self) -> &Path {
        &self.path
    }

    fn source(&self) -> &BinarySource {
        &self.source
    }
}

impl Resolvable for Bun {
    const EXE_NAME: &'static str = BUN_EXE;
    const ENV_VAR: Option<&'static str> = Some("APX_BUN_PATH");
    const PINNED_VERSION: Option<&'static str> = Some(BUN_VERSION);
    const VERSION_MARKER: Option<&'static str> = Some(".bun-version");
    const INSTALL_HINT: &'static str = "Install bun (https://bun.sh) or set APX_BUN_PATH.";

    fn from_resolved(resolved: ResolvedBinary) -> Self {
        Self {
            path: resolved.path,
            source: resolved.source,
        }
    }

    async fn download() -> Result<ResolvedBinary, String> {
        eprintln!("bun not found on PATH — downloading v{BUN_VERSION}...");
        let path = crate::download::download_bun().await.map_err(|e| {
            format!(
                "Failed to auto-install bun v{BUN_VERSION}: {e}\n  \
                 Install bun manually (https://bun.sh) or set APX_BUN_PATH."
            )
        })?;
        eprintln!("bun v{BUN_VERSION} installed to {}", path.display());
        Ok(ResolvedBinary {
            path,
            source: BinarySource::ApxManaged,
        })
    }
}

impl ToolInfo for Bun {
    async fn info() -> ToolInfoEntry {
        match resolve_local::<Self>() {
            Ok(resolved) => {
                let version = get_version(&resolved.path).await;
                ToolInfoEntry {
                    emoji: "\u{1f35e}",
                    name: "bun",
                    version: Some(version),
                    path: Some(resolved.path.display().to_string()),
                    source: Some(resolved.source.source_label().to_string()),
                    error: None,
                }
            }
            Err(e) => ToolInfoEntry {
                emoji: "\u{1f35e}",
                name: "bun",
                version: None,
                path: None,
                source: None,
                error: Some(e),
            },
        }
    }
}
