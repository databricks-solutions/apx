//! `databricks` CLI abstraction — wraps the Databricks CLI used in MCP tools.

use std::path::{Path, PathBuf};

use super::{
    BinarySource, CommandError, ExternalTool, Resolvable, ResolvedBinary, ToolCommand, ToolInfo,
    ToolInfoEntry, get_version, resolve_local,
};

#[cfg(target_os = "windows")]
const DATABRICKS_EXE: &str = "databricks.exe";
#[cfg(not(target_os = "windows"))]
const DATABRICKS_EXE: &str = "databricks";

/// A resolved `databricks` CLI binary.
#[derive(Debug, Clone)]
pub struct DatabricksCli {
    path: PathBuf,
    source: BinarySource,
}

impl DatabricksCli {
    /// Resolve `databricks` from PATH via the [`Resolvable`] trait.
    pub fn new() -> Result<Self, CommandError> {
        super::resolve_local::<Self>()
            .map(Self::from_resolved)
            .map_err(|_| CommandError::NotFound {
                tool: "databricks",
                hint: "install Databricks CLI v0.280.0+ and ensure it's on PATH",
            })
    }

    /// Create a `ToolCommand` for the databricks binary.
    pub fn cmd(&self) -> ToolCommand {
        ToolCommand::new(self.path.clone(), "databricks")
    }
}

impl ExternalTool for DatabricksCli {
    const NAME: &'static str = "databricks";

    fn binary_path(&self) -> &Path {
        &self.path
    }

    fn source(&self) -> &BinarySource {
        &self.source
    }
}

impl Resolvable for DatabricksCli {
    const EXE_NAME: &'static str = DATABRICKS_EXE;
    const ENV_VAR: Option<&'static str> = None;
    const PINNED_VERSION: Option<&'static str> = None;
    const VERSION_MARKER: Option<&'static str> = None;
    const INSTALL_HINT: &'static str = "Install Databricks CLI v0.280.0+ and ensure it's on PATH.";

    fn from_resolved(resolved: ResolvedBinary) -> Self {
        Self {
            path: resolved.path,
            source: resolved.source,
        }
    }
}

impl ToolInfo for DatabricksCli {
    async fn info() -> ToolInfoEntry {
        match resolve_local::<Self>() {
            Ok(resolved) => {
                let version = get_version(&resolved.path).await;
                ToolInfoEntry {
                    emoji: "\u{1f9f1}",
                    name: "databricks",
                    version: Some(version),
                    path: Some(resolved.path.display().to_string()),
                    source: Some(resolved.source.source_label().to_string()),
                    error: None,
                }
            }
            Err(e) => ToolInfoEntry {
                emoji: "\u{1f9f1}",
                name: "databricks",
                version: None,
                path: None,
                source: None,
                error: Some(e),
            },
        }
    }
}
