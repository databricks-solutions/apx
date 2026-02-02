use clap::{Args, ValueEnum};
use dialoguer::Confirm;
use similar::{ChangeTag, TextDiff};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tera::Context;
use walkdir::WalkDir;

use crate::cli::common::{Assistant, Layout, Template};
use crate::cli::run_cli_async;
use crate::interop::templates_dir;

/// Available addons that can be applied
#[derive(ValueEnum, Clone, Debug, Copy)]
#[value(rename_all = "lower")]
pub enum Addon {
    // Assistant addons (from common::Assistant)
    /// Cursor AI assistant rules
    Cursor,
    /// VSCode AI assistant rules
    Vscode,
    /// Claude AI assistant rules
    Claude,
    /// Codex AI assistant rules
    Codex,

    // Template addons (from common::Template)
    /// Stateful addon with database support
    Stateful,
    /// Essential/base template files
    Essential,

    // Layout addons (from common::Layout)
    /// Sidebar layout addon
    Sidebar,
}

impl Addon {
    /// Get the directory name for this addon in the templates folder
    fn directory_name(&self) -> &str {
        match self {
            // Assistant addons
            Addon::Cursor => Assistant::Cursor.directory_name(),
            Addon::Vscode => Assistant::Vscode.directory_name(),
            Addon::Claude => Assistant::Claude.directory_name(),
            Addon::Codex => Assistant::Codex.directory_name(),
            // Template addons
            Addon::Stateful => Template::Stateful.directory_name(),
            Addon::Essential => Template::Essential.directory_name(),
            // Layout addons
            Addon::Sidebar => Layout::Sidebar.directory_name().unwrap_or("sidebar"),
        }
    }

    /// Check if this addon is the base template (not in addons folder)
    fn is_base(&self) -> bool {
        matches!(self, Addon::Essential)
    }
}

#[derive(Args, Debug, Clone)]
pub struct ApplyArgs {
    /// The addon to apply
    #[arg(value_enum)]
    pub addon: Addon,

    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,

    #[arg(
        long = "yes",
        short = 'y',
        help = "Skip confirmation prompt and apply changes automatically"
    )]
    pub yes: bool,
}

pub async fn run(args: ApplyArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

/// Represents a file that will be created or modified
#[derive(Debug)]
struct FileChange {
    /// Relative path from app root
    rel_path: String,
    /// New content to write
    new_content: String,
    /// Existing content (None if file doesn't exist)
    existing_content: Option<String>,
}

impl FileChange {
    fn is_new(&self) -> bool {
        self.existing_content.is_none()
    }

    fn is_modified(&self) -> bool {
        match &self.existing_content {
            Some(existing) => existing != &self.new_content,
            None => false,
        }
    }

    /// Generate a unified diff for this file change
    fn generate_diff(&self) -> Option<String> {
        let existing = self.existing_content.as_ref()?;
        if existing == &self.new_content {
            return None;
        }

        let diff = TextDiff::from_lines(existing, &self.new_content);
        let mut output = String::new();

        output.push_str(&format!("\x1b[1m--- {} (current)\x1b[0m\n", self.rel_path));
        output.push_str(&format!("\x1b[1m+++ {} (new)\x1b[0m\n", self.rel_path));

        for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
            if idx > 0 {
                output.push_str("...\n");
            }
            for op in group {
                for change in diff.iter_changes(op) {
                    let (sign, color) = match change.tag() {
                        ChangeTag::Delete => ("-", "\x1b[31m"),
                        ChangeTag::Insert => ("+", "\x1b[32m"),
                        ChangeTag::Equal => (" ", ""),
                    };
                    output.push_str(color);
                    output.push_str(sign);
                    output.push_str(change.value());
                    if change.missing_newline() {
                        output.push('\n');
                    }
                    if !color.is_empty() {
                        output.push_str("\x1b[0m");
                    }
                }
            }
        }

        Some(output)
    }
}

async fn run_inner(args: ApplyArgs) -> Result<(), String> {
    let app_dir = args
        .app_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Read project context
    let (app_name, app_slug) = read_project_context(&app_dir)?;

    // Get templates directory
    let templates_dir = templates_dir()?;

    // Get addon source directory
    let addon_source = if args.addon.is_base() {
        templates_dir.join(args.addon.directory_name())
    } else {
        templates_dir
            .join("addons")
            .join(args.addon.directory_name())
    };

    if !addon_source.exists() {
        return Err(format!(
            "Addon '{}' not found at {}",
            args.addon.directory_name(),
            addon_source.display()
        ));
    }

    println!(
        "Applying {} addon to {}...\n",
        args.addon.directory_name(),
        app_dir
            .canonicalize()
            .unwrap_or_else(|_| app_dir.clone())
            .display()
    );

    // Collect all file changes
    let changes = collect_file_changes(&addon_source, &app_dir, &app_name, &app_slug)?;

    if changes.is_empty() {
        println!("No changes to apply.");
        return Ok(());
    }

    // Separate new files and modified files
    let new_files: Vec<_> = changes.iter().filter(|c| c.is_new()).collect();
    let modified_files: Vec<_> = changes.iter().filter(|c| c.is_modified()).collect();
    let unchanged_count = changes.len() - new_files.len() - modified_files.len();

    // Display summary
    if !new_files.is_empty() {
        println!("\x1b[32mFiles to be created:\x1b[0m");
        for file in &new_files {
            println!("  \x1b[32m+\x1b[0m {}", file.rel_path);
        }
        println!();
    }

    if !modified_files.is_empty() {
        println!("\x1b[33mFiles to be modified:\x1b[0m");
        for file in &modified_files {
            println!("  \x1b[33m~\x1b[0m {}", file.rel_path);
        }
        println!();

        // Show diffs for modified files
        println!("\x1b[1m--- Diffs ---\x1b[0m\n");
        for file in &modified_files {
            if let Some(diff) = file.generate_diff() {
                println!("{}", diff);
                println!();
            }
        }
    }

    if unchanged_count > 0 {
        println!("\x1b[90m{} file(s) unchanged\x1b[0m\n", unchanged_count);
    }

    // Summary line
    let total_changes = new_files.len() + modified_files.len();
    println!(
        "Summary: {} new, {} modified, {} unchanged",
        new_files.len(),
        modified_files.len(),
        unchanged_count
    );

    if total_changes == 0 {
        println!("All files are up to date.");
        return Ok(());
    }

    // Ask for confirmation unless -y flag is provided
    if !args.yes {
        let confirmed = Confirm::new()
            .with_prompt("Do you want to apply these changes?")
            .default(true)
            .interact()
            .map_err(|err| format!("Failed to read confirmation: {err}"))?;

        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Apply changes
    let mut created = 0;
    let mut modified = 0;

    for change in &changes {
        if change.is_new() || change.is_modified() {
            let target_path = app_dir.join(&change.rel_path);
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("Failed to create directory: {err}"))?;
            }
            fs::write(&target_path, &change.new_content)
                .map_err(|err| format!("Failed to write {}: {err}", change.rel_path))?;

            if change.is_new() {
                created += 1;
            } else {
                modified += 1;
            }
        }
    }

    println!(
        "\n\x1b[32m✓\x1b[0m Applied {} addon: {} file(s) created, {} file(s) modified",
        args.addon.directory_name(),
        created,
        modified
    );

    Ok(())
}

/// Read project context (app_name and app_slug) from pyproject.toml
fn read_project_context(app_dir: &Path) -> Result<(String, String), String> {
    let pyproject_path = app_dir.join("pyproject.toml");

    if !pyproject_path.exists() {
        return Err(format!(
            "pyproject.toml not found at {}. Are you in an apx project directory?",
            pyproject_path.display()
        ));
    }

    let content = fs::read_to_string(&pyproject_path)
        .map_err(|err| format!("Failed to read pyproject.toml: {err}"))?;

    let doc: toml::Value = content
        .parse()
        .map_err(|err| format!("Failed to parse pyproject.toml: {err}"))?;

    let app_name = doc
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or("Could not find project.name in pyproject.toml")?
        .to_string();

    // Convert app_name to app_slug (replace - with _)
    let app_slug = app_name.replace('-', "_");

    Ok((app_name, app_slug))
}

/// Collect all file changes from the addon source directory
fn collect_file_changes(
    source_dir: &Path,
    target_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<Vec<FileChange>, String> {
    let mut changes = Vec::new();

    for entry in WalkDir::new(source_dir) {
        let entry = entry.map_err(|err| format!("Failed to read template directory: {err}"))?;
        if !entry.file_type().is_file() {
            continue;
        }

        let rel_path = entry
            .path()
            .strip_prefix(source_dir)
            .map_err(|err| format!("Failed to build relative path: {err}"))?;

        let mut path_str = rel_path.to_string_lossy().replace('\\', "/");

        // Replace "base" with app_slug in paths
        if path_str.contains("/base/") || path_str.starts_with("base/") {
            path_str = path_str
                .replace("/base/", &format!("/{app_slug}/"))
                .replace("base/", &format!("{app_slug}/"));
        }

        let is_template = entry.path().extension() == Some(OsStr::new("jinja2"));
        let final_rel_path = if is_template {
            path_str.trim_end_matches(".jinja2").to_string()
        } else {
            path_str
        };

        let target_path = target_dir.join(&final_rel_path);

        // Generate new content
        let new_content = if is_template {
            let template_content = fs::read_to_string(entry.path())
                .map_err(|err| format!("Failed to read template: {err}"))?;

            let mut context = Context::new();
            context.insert("app_name", app_name);
            context.insert("app_slug", app_slug);
            context.insert(
                "app_letter",
                &app_name.chars().next().unwrap_or('A').to_string(),
            );

            tera::Tera::one_off(&template_content, &context, false).map_err(|err| {
                format!(
                    "Failed to render template {}: {err}",
                    entry.path().display()
                )
            })?
        } else {
            fs::read_to_string(entry.path())
                .map_err(|err| format!("Failed to read file {}: {err}", entry.path().display()))?
        };

        // Read existing content if file exists
        let existing_content = if target_path.exists() {
            Some(
                fs::read_to_string(&target_path)
                    .map_err(|err| format!("Failed to read existing file: {err}"))?,
            )
        } else {
            None
        };

        changes.push(FileChange {
            rel_path: final_rel_path,
            new_content,
            existing_content,
        });
    }

    // Sort by path for consistent output
    changes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    Ok(changes)
}
