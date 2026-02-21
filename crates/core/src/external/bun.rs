//! `bun` binary abstraction — replaces [`BunCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use super::{BinarySource, ExternalTool, Resolvable, ResolvedBinary, resolve_with_download};

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
    pub async fn resolve() -> Result<Self, String> {
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

    /// Create a `tokio::process::Command` for spawning bun (with patched PATH).
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.env("PATH", self.patched_path());
        cmd
    }

    /// Create a `std::process::Command` for spawning bun (with patched PATH).
    pub fn std_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.path);
        cmd.env("PATH", self.patched_path());
        cmd
    }

    /// Create a tokio command with `NODE_PATH` set to `<app_dir>/node_modules`.
    ///
    /// Use this when running scripts that live outside the project directory
    /// (e.g. the bundled entrypoint.ts at ~/.apx/files/). Without NODE_PATH,
    /// bun resolves transitive dependencies relative to the script's location
    /// or its global cache, which fails to find packages installed in the
    /// project's node_modules.
    pub fn tokio_command_with_node_path(&self, app_dir: &Path) -> tokio::process::Command {
        let mut cmd = self.tokio_command();
        cmd.env("NODE_PATH", app_dir.join("node_modules"));
        cmd
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
