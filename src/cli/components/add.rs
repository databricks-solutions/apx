use clap::Args;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::{bun_binary_path, cli::run_cli_async};
use crate::common::read_project_metadata;

use super::{plan_add, AddPlan, PlannedFile, collect_css_mutations, apply_css_updates, UiConfig};
use crate::cli::components::utils::format_relative_path;

fn resolve_app_dir(app_path: Option<PathBuf>) -> PathBuf {
    app_path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn print_plan_summary(plan: &AddPlan) {
    println!("Components:");
    for component in &plan.components {
        let registry = component.registry.as_deref().unwrap_or("default");
        println!("  - {} ({})", component.name, registry);
    }

    println!("Files:");
    for file in &plan.files_to_write {
        println!(
            "  {} (from {})",
            file.relative_path.display(),
            file.source_component
        );
    }

    if !plan.component_deps.is_empty() {
        println!("Dependencies:");
        for dep in &plan.component_deps {
            println!("  - {}", dep);
        }
    }

    if !plan.warnings.is_empty() {
        println!("Warnings:");
        for warning in &plan.warnings {
            let indented = warning.replace('\n', "\n    ");
            println!("  - {}", indented);
        }
    }
}

enum WriteResult {
    Written,
    Unchanged,
}

fn write_file_if_changed(
    file: &PlannedFile,
    force: bool,
    app_dir: &Path,
) -> Result<WriteResult, String> {
    if file.absolute_path.exists() {
        let existing = std::fs::read_to_string(&file.absolute_path)
            .map_err(|e| format!("Failed to read {}: {e}", file.absolute_path.display()))?;
        if existing == file.content {
            return Ok(WriteResult::Unchanged);
        }
        if !force {
            return Err(format!(
                "File already exists (use --force): {}",
                format_relative_path(&file.absolute_path, app_dir)
            ));
        }
    }

    if let Some(parent) = file.absolute_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    std::fs::write(&file.absolute_path, &file.content)
        .map_err(|e| format!("Failed to write {}: {e}", file.absolute_path.display()))?;
    Ok(WriteResult::Written)
}

#[derive(Args, Debug, Clone)]
pub struct ComponentsAddArgs {
    /// Component name (e.g. button, dialog)
    pub component: String,

    /// Registry name (from components.json)
    #[arg(long)]
    pub registry: Option<String>,

    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,

    /// Print actions without writing files
    #[arg(long)]
    pub dry_run: bool,

    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
}

pub async fn run(args: ComponentsAddArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

pub async fn run_inner(args: ComponentsAddArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path);
    
    // Load metadata from pyproject.toml
    let metadata = read_project_metadata(&app_dir)?;
    let cfg = UiConfig::from_metadata(&metadata, &app_dir);
    
    let client = reqwest::Client::new();
    
    // Parse component name to extract registry prefix if present (e.g., @animate-ui/button)
    let (registry, component) = if args.component.starts_with('@') && args.registry.is_none() {
        if let Some((prefix, name)) = args.component.split_once('/') {
            tracing::debug!(
                original = %args.component,
                registry = %prefix,
                component = %name,
                "Detected registry prefix in component name"
            );
            (Some(prefix.to_string()), name.to_string())
        } else {
            (args.registry, args.component.clone())
        }
    } else {
        (args.registry, args.component.clone())
    };

    let plan = plan_add(
        &client,
        &app_dir,
        &cfg,
        registry.as_deref(),
        &component,
    )
    .await?;

    if args.dry_run {
        print_plan_summary(&plan);
        return Ok(());
    }

    let mut written_paths = Vec::new();
    let mut unchanged_paths = Vec::new();
    for file in &plan.files_to_write {
        match write_file_if_changed(file, args.force, &app_dir)? {
            WriteResult::Written => written_paths.push(file.absolute_path.clone()),
            WriteResult::Unchanged => unchanged_paths.push(file.absolute_path.clone()),
        }
    }

    if !written_paths.is_empty() {
        println!("Written:");
        for path in &written_paths {
            println!("  {}", format_relative_path(path, &app_dir));
        }
    }

    if !unchanged_paths.is_empty() {
        println!("Unchanged:");
        for path in &unchanged_paths {
            println!("  {}", format_relative_path(path, &app_dir));
        }
    }

    if !plan.component_deps.is_empty() {
        let deps: Vec<String> = plan.component_deps.iter().cloned().collect();
        println!();
        println!("Installing dependencies:");
        println!("  {}", deps.join(" "));
        bun_add(&app_dir, &deps).await?;
        println!("Dependencies installed");
    }

    // Apply CSS updates automatically
    let css_mutations = collect_css_mutations(&plan.components);
    if !css_mutations.is_empty() {
        let css_path = app_dir.join(cfg.css_path());
        match apply_css_updates(&css_path, css_mutations) {
            Ok(()) => {
                println!();
                println!("Updated CSS file: {}", format_relative_path(&css_path, &app_dir));
            }
            Err(e) => {
                eprintln!("\nWARNING: Failed to automatically update CSS: {}", e);
                eprintln!("You may need to manually add CSS variables to your CSS file.");
            }
        }
    }

    for warning in &plan.warnings {
        eprintln!("\nWARNING: {}", warning);
    }

    Ok(())
}

async fn bun_add(app_dir: &Path, deps: &[String]) -> Result<(), String> {
    if deps.is_empty() {
        return Ok(());
    }

    let bun_path = bun_binary_path()?;
    let output = Command::new(bun_path)
        .arg("add")
        .args(deps)
        .current_dir(app_dir)
        .output()
        .await
        .map_err(|e| format!("Failed to install dependencies: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Failed to install dependencies. Stdout: {} Stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}
