use clap::Args;
use std::fs;
use std::path::{Path, PathBuf};

use crate::run_cli_async_helper;
use apx_core::interop::{get_template_content, list_template_files};

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    /// Install to ~/.claude/ (global) instead of .claude/ (project-level)
    #[arg(long)]
    pub global: bool,

    /// Directory where skill files are installed (relative to base dir)
    #[arg(long, default_value = ".claude/skills/apx")]
    pub path: String,
}

pub async fn run(args: InstallArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: InstallArgs) -> Result<(), String> {
    let base_dir = if args.global {
        home_dir()?
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Could not determine current directory: {e}"))?
    };

    let installed = install_skills_to(&base_dir, &args.path)?;

    if installed.is_empty() {
        return Err("No skill files found to install".into());
    }

    let location = if args.global {
        "globally"
    } else {
        "to project"
    };
    println!("\x1b[32m✓\x1b[0m Installed apx skill files {location}:");
    for f in &installed {
        println!("  {f}");
    }

    Ok(())
}

/// Install skill infrastructure files (skills, .mcp.json) to a target directory.
///
/// Reads embedded files from `addons/claude/` and writes:
/// - `.claude/skills/apx/*` → `{base_dir}/{skill_path}/`
/// - `.mcp.json` → `{base_dir}/.mcp.json`
///
/// Skips addon-specific files (addon.toml, templates, hooks, cursor/vscode/github configs).
pub fn install_skills_to(base_dir: &Path, skill_path: &str) -> Result<Vec<String>, String> {
    let prefix = "addons/claude/";
    let all_files = list_template_files(prefix);

    if all_files.is_empty() {
        return Err("No embedded skill files found".into());
    }

    let skill_source_prefix = ".claude/skills/apx/";
    let mut installed = Vec::new();

    for file_path in &all_files {
        let rel = file_path.strip_prefix(prefix).unwrap_or(file_path.as_str());

        // Determine output path based on file type
        let output_rel = if let Some(skill_rel) = rel.strip_prefix(skill_source_prefix) {
            // Skill markdown files: rebase from .claude/skills/apx/ to skill_path/
            format!("{}/{}", skill_path, skill_rel)
        } else if rel == ".mcp.json" {
            ".mcp.json".to_string()
        } else {
            // Skip addon.toml, CLAUDE.md.jinja2, and any other addon-specific files
            continue;
        };

        let target = base_dir.join(&output_rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {e}", parent.display()))?;
        }

        let content = get_template_content(file_path)?;
        fs::write(&target, content.as_bytes())
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;

        installed.push(output_rel);
    }

    Ok(installed)
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .map_err(|_| "Could not determine home directory (USERPROFILE not set)".into())
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| "Could not determine home directory (HOME not set)".into())
    }
}
