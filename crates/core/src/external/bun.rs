//! `bun` binary abstraction — replaces [`BunCommand`] from `common.rs`.

use std::path::{Path, PathBuf};

use crate::download::{BinarySource, ResolvedBinary, resolve_bun};

use super::ExternalTool;

/// A resolved `bun` binary.
#[derive(Debug, Clone)]
pub struct Bun {
    path: PathBuf,
    source: BinarySource,
}

impl Bun {
    /// Resolve bun binary (downloads if needed).
    pub async fn resolve() -> Result<Self, String> {
        let resolved = resolve_bun().await?;
        tracing::debug!(
            "using {} bun: {}",
            resolved.source_label(),
            resolved.path.display()
        );
        Ok(Self::from_resolved(resolved))
    }

    fn from_resolved(resolved: ResolvedBinary) -> Self {
        Self {
            path: resolved.path,
            source: resolved.source,
        }
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
