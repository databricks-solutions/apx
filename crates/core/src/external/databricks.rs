//! `databricks` CLI abstraction — wraps the Databricks CLI used in MCP tools.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use super::{
    BinarySource, CommandError, CommandOutput, ExternalTool, Resolvable, ResolvedBinary,
    ToolCommand, ToolInfo, ToolInfoEntry, get_version, resolve_local,
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

/// Result of a `databricks apps logs` invocation.
#[derive(Debug)]
pub struct AppsLogsResult {
    pub output: CommandOutput,
    pub command_args: Vec<String>,
    pub duration_ms: u64,
}

/// Arguments for `databricks apps logs`.
#[derive(Debug)]
pub struct AppsLogsArgs<'a> {
    pub app_name: &'a str,
    pub tail_lines: i32,
    pub search: Option<&'a str>,
    pub source: Option<&'a [String]>,
    pub profile: Option<&'a str>,
    pub target: Option<&'a str>,
    pub output_format: &'a str,
    pub timeout_secs: f64,
    pub cwd: &'a Path,
    pub env_vars: &'a HashMap<String, String>,
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

    /// Run `databricks apps logs <app_name>` with the given arguments.
    pub async fn apps_logs(&self, args: AppsLogsArgs<'_>) -> Result<AppsLogsResult, CommandError> {
        let mut cmd_args = vec![
            "apps".to_string(),
            "logs".to_string(),
            args.app_name.to_string(),
            "--tail-lines".to_string(),
            args.tail_lines.to_string(),
        ];

        let mut cmd = tokio::process::Command::new(&self.path);
        cmd.args(&cmd_args)
            .current_dir(args.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Optional flags
        let mut push_flag = |flag: &str, value: Option<&str>| {
            if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                cmd.arg(flag).arg(v);
                cmd_args.push(flag.to_string());
                cmd_args.push(v.to_string());
            }
        };
        push_flag("--search", args.search);
        push_flag("-p", args.profile);
        push_flag("-t", args.target);

        if let Some(sources) = args.source {
            for src in sources {
                cmd.arg("--source").arg(src);
                cmd_args.push("--source".to_string());
                cmd_args.push(src.clone());
            }
        }

        cmd.arg("-o").arg(args.output_format);
        cmd_args.push("-o".to_string());
        cmd_args.push(args.output_format.to_string());

        if !args.env_vars.is_empty() {
            cmd.envs(args.env_vars);
        }

        let start = std::time::Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs_f64(args.timeout_secs), cmd.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(output)) => Ok(AppsLogsResult {
                output: CommandOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    exit_code: output.status.code(),
                },
                command_args: cmd_args,
                duration_ms,
            }),
            Ok(Err(e)) => Err(CommandError::from_io(
                "databricks",
                "install Databricks CLI v0.280.0+ and ensure it's on PATH",
                e,
            )),
            Err(_) => Err(CommandError::Timeout {
                tool: "databricks",
                timeout_secs: args.timeout_secs,
            }),
        }
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
