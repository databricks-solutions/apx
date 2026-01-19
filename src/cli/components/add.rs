use clap::Args;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::{
    bun_binary_path,
    cli::{components::resolve_component_request, run_cli_async},
};

use super::{fetch_component, load_components_json, resolve_ui_base_dir, write_component_files};

/// Format a path as relative to the app directory, with ./ prefix and cleaned up ././ patterns
fn format_relative_path(path: &Path, app_dir: &Path) -> String {
    path.strip_prefix(app_dir)
        .map(format_relative_string)
        .unwrap_or_else(|_| path.display().to_string())
}

fn format_relative_string(path: &Path) -> String {
    let mut s = path.to_string_lossy().to_string();
    // Ensure it starts with ./
    if !s.starts_with('.') {
        s.insert_str(0, "./");
    }
    // Clean up ././ patterns
    while s.contains("././") {
        s = s.replace("././", "./");
    }
    s
}

fn resolve_app_dir(app_path: Option<PathBuf>) -> PathBuf {
    app_path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn print_dependencies(dependencies: &[String]) {
    if dependencies.is_empty() {
        return;
    }

    println!("Dependencies:");
    for dependency in dependencies {
        println!("  - {}", dependency);
    }
}

fn print_written_paths(paths: &[PathBuf], app_dir: &Path) {
    for path in paths {
        println!("  {}", format_relative_path(path, app_dir));
    }
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

async fn run_inner(args: ComponentsAddArgs) -> Result<(), String> {
    let app_dir = resolve_app_dir(args.app_path);

    let cfg = load_components_json(&app_dir)?;
    let ui_base = resolve_ui_base_dir(&app_dir, &cfg)?;

    let client = reqwest::Client::new();

    let req = resolve_component_request(&cfg, args.registry.as_deref(), &args.component)?;
    let spec = fetch_component(&client, &req).await?;

    if args.dry_run {
        println!("Component: {}", spec.name);
        println!("Registry URL: {}", req.url);
        println!(
            "Target directory: {}",
            format_relative_path(&ui_base, &app_dir)
        );
        println!("Files:");
        let file_paths: Vec<PathBuf> = spec.files.iter().map(|f| ui_base.join(&f.path)).collect();
        print_written_paths(&file_paths, &app_dir);
        print_dependencies(&spec.dependencies);

        return Ok(());
    }

    let written = write_component_files(&ui_base, &spec, args.force)?;

    println!("Added component -> {}", spec.name);
    print_written_paths(&written, &app_dir);

    if spec.dependencies.is_empty() {
        return Ok(());
    }

    println!();
    println!("Installing dependencies:");
    println!("  {}", spec.dependencies.join(" "));

    let bun_path = bun_binary_path()?;
    let output = Command::new(bun_path)
        .arg("add")
        .args(&spec.dependencies)
        .current_dir(&app_dir)
        .output()
        .await
        .map_err(|e| format!("Failed to install dependencies: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Failed to install dependencies: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    println!("Dependencies installed");

    Ok(())
}
