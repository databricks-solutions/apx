use clap::Args;
use std::fs;
use std::path::PathBuf;

use crate::run_cli_async_helper;
use apx_core::interop::{get_template_content, list_template_files};

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    /// Install to ~/.claude/ (global) instead of .claude/ (project-level)
    #[arg(long)]
    pub global: bool,
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

    let prefix = "addons/claude/";
    let all_files = list_template_files(prefix);

    if all_files.is_empty() {
        return Err("No embedded skill files found".into());
    }

    let mut installed = Vec::new();

    for file_path in &all_files {
        let rel = file_path.strip_prefix(prefix).unwrap_or(file_path.as_str());

        // Skip addon.toml (internal metadata) and .gitignore
        if rel == "addon.toml" || rel == ".gitignore" {
            continue;
        }

        let target = base_dir.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {e}", parent.display()))?;
        }

        let content = get_template_content(file_path)?;
        fs::write(&target, content.as_bytes())
            .map_err(|e| format!("Failed to write {}: {e}", target.display()))?;

        installed.push(rel.to_string());
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
