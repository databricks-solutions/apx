//! Common types shared across CLI commands

use clap::ValueEnum;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;
use tracing::debug;
use walkdir::WalkDir;

const MAX_DISCOVERY_DEPTH: usize = 5;

/// Resolve the app directory from an optional path argument.
/// Falls back to the current working directory, or "." if that fails.
/// Used by `init` which creates new projects and does not need discovery.
pub fn resolve_app_dir(app_path: Option<PathBuf>) -> PathBuf {
    app_path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Find an existing apx project directory, with automatic discovery.
///
/// If an explicit path is given, returns it directly.
/// Otherwise checks CWD for a `pyproject.toml` with `[tool.apx]`, and if not
/// found, scans subdirectories up to 5 levels deep.
pub fn find_app_dir(app_path: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = app_path {
        return Ok(path);
    }

    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to determine current directory: {e}"))?;

    if has_apx_config(&cwd.join("pyproject.toml")) {
        debug!("apx config found in current directory");
        return Ok(cwd);
    }

    discover_apx_project(&cwd)
}

/// Search subdirectories up to 5 levels deep for an apx project.
fn discover_apx_project(root: &Path) -> Result<PathBuf, String> {
    debug!("searching for apx projects in {}", root.display());

    let mut found: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(MAX_DISCOVERY_DEPTH + 1)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && name != "node_modules" && name != "__pycache__"
        })
        .flatten()
    {
        if entry.file_type().is_file() && entry.file_name() == "pyproject.toml" {
            let depth = entry.depth();
            debug!(
                "checking pyproject.toml at depth {}: {}",
                depth,
                entry.path().display()
            );
            if has_apx_config(entry.path())
                && let Some(parent) = entry.path().parent()
            {
                found.push(parent.to_path_buf());
            }
        }
    }

    match found.len() {
        0 => Err("apx folder not found".to_string()),
        1 => {
            debug!("discovered apx project at {}", found[0].display());
            Ok(found.remove(0))
        }
        _ => {
            let paths = found
                .iter()
                .map(|p| format!("- {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!(
                "apx application identified in several sub-folders, \
                 please explicitly specify the one you would like to use:\n{paths}"
            ))
        }
    }
}

/// Check whether a `pyproject.toml` file contains a `[tool.apx]` section.
pub(crate) fn has_apx_config(pyproject_path: &Path) -> bool {
    fs::read_to_string(pyproject_path)
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok())
        .and_then(|v| v.get("tool")?.get("apx").cloned())
        .is_some()
}

/// Read, mutate, and write back a `pyproject.toml` via `toml_edit`.
pub(crate) fn modify_pyproject(
    path: &Path,
    f: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    let contents =
        fs::read_to_string(path).map_err(|e| format!("Failed to read pyproject.toml: {e}"))?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Invalid TOML: {e}"))?;
    f(&mut doc)?;
    fs::write(path, doc.to_string()).map_err(|e| format!("Failed to write pyproject.toml: {e}"))?;
    Ok(())
}

/// Project template types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Template {
    /// Minimal template with basic UI structure
    Minimal,
    /// Standard template with UI and API
    Essential,
    /// Template with database integration
    Stateful,
}

impl Template {
    /// Get the directory name for this template addon
    pub fn directory_name(&self) -> &str {
        match self {
            Template::Minimal => "minimal-ui",
            Template::Essential => "base",
            Template::Stateful => "stateful",
        }
    }
}

/// AI assistant configuration types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Assistant {
    /// Cursor IDE rules and MCP config
    Cursor,
    /// VSCode instructions and MCP config
    Vscode,
    /// OpenAI Codex AGENTS.md file
    Codex,
    /// Claude project file and MCP config
    Claude,
}

impl Assistant {
    /// Get the directory name for this assistant addon
    pub fn directory_name(&self) -> &str {
        match self {
            Assistant::Cursor => "cursor",
            Assistant::Vscode => "vscode",
            Assistant::Codex => "codex",
            Assistant::Claude => "claude",
        }
    }
}

/// UI layout types
#[derive(ValueEnum, Clone, Debug, Copy, PartialEq, Eq)]
#[value(rename_all = "lower")]
pub enum Layout {
    /// Basic layout without sidebar
    Basic,
    /// Sidebar navigation layout
    Sidebar,
}

impl Layout {
    /// Get the directory name for this layout addon (None for Basic)
    pub fn directory_name(&self) -> Option<&str> {
        match self {
            Layout::Basic => None,
            Layout::Sidebar => Some("sidebar"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const APX_PYPROJECT: &str = r#"[project]
name = "my-app"

[tool.apx.metadata]
app-name = "my-app"
"#;

    const PLAIN_PYPROJECT: &str = r#"[project]
name = "other-project"
version = "0.1.0"
"#;

    #[test]
    fn test_has_apx_config_true() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, APX_PYPROJECT).unwrap();
        assert!(has_apx_config(&path));
    }

    #[test]
    fn test_has_apx_config_false() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, PLAIN_PYPROJECT).unwrap();
        assert!(!has_apx_config(&path));
    }

    #[test]
    fn test_has_apx_config_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(!has_apx_config(&dir.path().join("pyproject.toml")));
    }

    #[test]
    fn test_discover_finds_single_apx_project() {
        let root = TempDir::new().unwrap();
        let pkg = root.path().join("packages").join("app");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let result = discover_apx_project(root.path()).unwrap();
        assert_eq!(result, pkg);
    }

    #[test]
    fn test_discover_errors_on_multiple_projects() {
        let root = TempDir::new().unwrap();

        let pkg1 = root.path().join("packages").join("app1");
        fs::create_dir_all(&pkg1).unwrap();
        fs::write(pkg1.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let pkg2 = root.path().join("packages").join("app2");
        fs::create_dir_all(&pkg2).unwrap();
        fs::write(pkg2.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let err = discover_apx_project(root.path()).unwrap_err();
        assert!(err.contains("several sub-folders"), "got: {err}");
        assert!(err.contains("app1"), "got: {err}");
        assert!(err.contains("app2"), "got: {err}");
    }

    #[test]
    fn test_discover_errors_when_none_found() {
        let root = TempDir::new().unwrap();
        // Empty directory -- no apx projects
        let err = discover_apx_project(root.path()).unwrap_err();
        assert!(err.contains("apx folder not found"), "got: {err}");
    }

    #[test]
    fn test_discover_ignores_non_apx_pyproject() {
        let root = TempDir::new().unwrap();
        let pkg = root.path().join("packages").join("lib");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("pyproject.toml"), PLAIN_PYPROJECT).unwrap();

        let err = discover_apx_project(root.path()).unwrap_err();
        assert!(err.contains("apx folder not found"), "got: {err}");
    }

    #[test]
    fn test_discover_respects_max_depth() {
        let root = TempDir::new().unwrap();
        // 6 directory levels deep -- exceeds MAX_DISCOVERY_DEPTH (5), should NOT be found
        let deep = root.path().join("a/b/c/d/e/f");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let err = discover_apx_project(root.path()).unwrap_err();
        assert!(err.contains("apx folder not found"), "got: {err}");
    }

    #[test]
    fn test_discover_finds_at_max_depth() {
        let root = TempDir::new().unwrap();
        // Depth 5: a/b/c/d/e/pyproject.toml -- should be found
        let deep = root.path().join("a/b/c/d/e");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let result = discover_apx_project(root.path()).unwrap();
        assert_eq!(result, deep);
    }

    #[test]
    fn test_discover_skips_hidden_dirs() {
        let root = TempDir::new().unwrap();
        let hidden = root.path().join(".hidden").join("app");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("pyproject.toml"), APX_PYPROJECT).unwrap();

        let err = discover_apx_project(root.path()).unwrap_err();
        assert!(err.contains("apx folder not found"), "got: {err}");
    }

    #[test]
    fn test_find_app_dir_explicit_path_skips_discovery() {
        let explicit = PathBuf::from("/some/explicit/path");
        let result = find_app_dir(Some(explicit.clone())).unwrap();
        assert_eq!(result, explicit);
    }

    #[test]
    fn test_modify_pyproject_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, PLAIN_PYPROJECT).unwrap();

        modify_pyproject(&path, |doc| {
            doc["tool"]["custom"] = toml_edit::Item::Value("hello".into());
            Ok(())
        })
        .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[project]"));
        assert!(content.contains("name = \"other-project\""));
        assert!(content.contains("custom = \"hello\""));
    }
}
