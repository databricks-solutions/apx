use clap::{Args, ValueEnum};
use dialoguer::{Confirm, Input, Select};
use rand::seq::SliceRandom;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tera::Context;
use tokio::process::Command;
use tracing::debug;
use walkdir::WalkDir;

use crate::cli::components::add::{ComponentInput, add_components};
use crate::cli::run_cli_async;
use crate::common::list_profiles;
use crate::common::{
    BunCommand, format_elapsed_ms, run_with_spinner, run_with_spinner_async, spinner,
};
use crate::dotenv::DotenvFile;
use crate::interop::templates_dir;
use std::time::Instant;

const APX_INDEX_URL: &str = "https://databricks-solutions.github.io/apx/simple";

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
pub enum Template {
    Minimal,
    Essential,
    Stateful,
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
pub enum Assistant {
    Cursor,
    Vscode,
    Codex,
    Claude,
}

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
pub enum Layout {
    Basic,
    Sidebar,
}

#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    #[arg(
        value_name = "APP_PATH",
        help = "The path to the app. Defaults to current working directory"
    )]
    pub app_path: Option<PathBuf>,
    #[arg(
        long = "name",
        short = 'n',
        help = "The name of the project. Will prompt if not provided"
    )]
    pub app_name: Option<String>,
    #[arg(
        long,
        short = 't',
        value_enum,
        help = "The template to use. Will prompt if not provided"
    )]
    pub template: Option<Template>,
    #[arg(
        long,
        short = 'p',
        help = "The Databricks profile to use. Will prompt if not provided"
    )]
    pub profile: Option<String>,
    #[arg(
        long,
        short = 'a',
        value_enum,
        help = "The type of assistant to use (cursor/vscode/codex/claude). Will prompt if not provided"
    )]
    pub assistant: Option<Assistant>,
    #[arg(
        long,
        short = 'l',
        value_enum,
        help = "The layout to use. Will prompt if not provided"
    )]
    pub layout: Option<Layout>,
}

pub async fn run(args: InitArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(mut args: InitArgs) -> Result<(), String> {
    if !is_command_available("uv").await {
        return Err("uv is not installed. Please install uv to continue.".to_string());
    }

    let bun = BunCommand::new()?;
    if !bun.exists() {
        return Err("bun is not installed. Please install bun to continue.".to_string());
    }

    let app_path = args
        .app_path
        .take()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let templates_dir = templates_dir()?;

    println!("Welcome to apx 🚀\n");

    if args.app_name.is_none() {
        let default_name = random_name();
        let name = Input::<String>::new()
            .with_prompt("What's the name of your app?")
            .default(default_name)
            .interact_text()
            .map_err(|err| format!("Failed to read app name: {err}"))?;
        args.app_name = Some(name);
    }

    let app_name_raw = args.app_name.take().unwrap_or_default();
    let app_name = normalize_app_name(&app_name_raw)?;
    let app_slug = app_name.replace("-", "_");

    if args.template.is_none() {
        let choices = [Template::Minimal, Template::Essential, Template::Stateful];
        let default_idx = 1; // Default to essential
        let selection = Select::new()
            .with_prompt("Which template would you like to use?")
            .items(&["minimal", "essential", "stateful"])
            .default(default_idx)
            .interact()
            .map_err(|err| format!("Failed to select template: {err}"))?;
        args.template = Some(choices[selection].clone());
    }

    if args.profile.is_none() {
        let available_profiles = list_profiles()?;
        if !available_profiles.is_empty() {
            println!(
                "Available Databricks profiles: {}",
                available_profiles.join(", ")
            );
            let profile_input = Input::<String>::new()
                .with_prompt(
                    "Which Databricks profile would you like to use? (leave empty to skip)",
                )
                .allow_empty(true)
                .interact_text()
                .map_err(|err| format!("Failed to read profile: {err}"))?;
            if profile_input.trim().is_empty() {
                args.profile = None;
            } else {
                args.profile = Some(profile_input);
            }
        } else {
            println!("No Databricks profiles found in ~/.databrickscfg");
            let should_prompt = Confirm::new()
                .with_prompt("Would you like to specify a profile name?")
                .default(false)
                .interact()
                .map_err(|err| format!("Failed to read profile choice: {err}"))?;
            if should_prompt {
                let profile = Input::<String>::new()
                    .with_prompt("Enter profile name")
                    .interact_text()
                    .map_err(|err| format!("Failed to read profile: {err}"))?;
                args.profile = Some(profile);
            } else {
                args.profile = None;
            }
        }
    }

    if args.assistant.is_none() {
        let should_setup = Confirm::new()
            .with_prompt("Would you like to set up AI assistant rules?")
            .default(true)
            .interact()
            .map_err(|err| format!("Failed to read assistant choice: {err}"))?;
        if should_setup {
            let choices = [
                Assistant::Cursor,
                Assistant::Vscode,
                Assistant::Codex,
                Assistant::Claude,
            ];
            let selection = Select::new()
                .with_prompt("Which assistant would you like to use?")
                .items(&["cursor", "vscode", "codex", "claude"])
                .default(0)
                .interact()
                .map_err(|err| format!("Failed to select assistant: {err}"))?;
            args.assistant = Some(choices[selection].clone());
        }
    }

    let template = args.template.take().unwrap_or(Template::Essential);

    // Skip layout selection for minimal template (always uses basic layout)
    if !matches!(template, Template::Minimal) && args.layout.is_none() {
        let choices = [Layout::Sidebar, Layout::Basic];
        let selection = Select::new()
            .with_prompt("Which layout would you like to use?")
            .items(&["sidebar", "basic"])
            .default(0)
            .interact()
            .map_err(|err| format!("Failed to select layout: {err}"))?;
        args.layout = Some(choices[selection].clone());
    }

    // Minimal template always uses basic layout
    let layout = if matches!(template, Template::Minimal) {
        Layout::Basic
    } else {
        args.layout.take().unwrap_or(Layout::Sidebar)
    };

    println!(
        "\nInitializing app {} in {}\n",
        app_name,
        app_path
            .canonicalize()
            .unwrap_or_else(|_| app_path.clone())
            .display()
    );

    run_with_spinner(
        "📁 Preparing project layout...",
        "✅ Project layout prepared",
        || {
            ensure_dir(&app_path)?;
            let base_template_dir = templates_dir.join("base");
            process_template_directory(&base_template_dir, &app_path, &app_name, &app_slug)?;

            let dist_dir = app_path.join("src").join(&app_slug).join("__dist__");
            ensure_dir(&dist_dir)?;
            fs::write(dist_dir.join(".gitignore"), "*\n")
                .map_err(|err| format!("Failed to write dist .gitignore: {err}"))?;

            let build_dir = app_path.join(".build");
            ensure_dir(&build_dir)?;
            fs::write(build_dir.join(".gitignore"), "*\n")
                .map_err(|err| format!("Failed to write .build .gitignore: {err}"))?;

            if matches!(template, Template::Stateful) {
                let stateful_addon = templates_dir.join("addons").join("stateful");
                process_template_directory(&stateful_addon, &app_path, &app_name, &app_slug)?;
            }

            // Apply minimal UI overlay and cleanup unused files
            if matches!(template, Template::Minimal) {
                let minimal_ui_addon = templates_dir.join("addons").join("minimal-ui");
                process_template_directory(&minimal_ui_addon, &app_path, &app_name, &app_slug)?;

                // Delete unused files for minimal template
                let ui_path = app_path.join("src").join(&app_slug).join("ui");
                // Remove components/ui/ (shadcn)
                let _ = fs::remove_dir_all(ui_path.join("components/ui"));
                // Remove components/backgrounds/
                let _ = fs::remove_dir_all(ui_path.join("components/backgrounds"));
                // Remove unused apx components
                let _ = fs::remove_file(ui_path.join("components/apx/mode-toggle.tsx"));
                let _ = fs::remove_file(ui_path.join("components/apx/navbar.tsx"));
                let _ = fs::remove_file(ui_path.join("components/apx/theme-provider.tsx"));
            }

            if let Some(profile) = args.profile.as_deref() {
                let mut dotenv = DotenvFile::read(&app_path.join(".env"))?;
                dotenv.update("DATABRICKS_CONFIG_PROFILE", profile)?;
            }

            if matches!(layout, Layout::Sidebar) {
                let sidebar_addon = templates_dir.join("addons").join("sidebar");
                process_template_directory(&sidebar_addon, &app_path, &app_name, &app_slug)?;
            }
            Ok(())
        },
    )?;

    // Git initialization logic
    if !is_command_available("git").await {
        println!("⚠️  Git is not available - skipping git initialization");
    } else if is_in_git_repo(&app_path).await? {
        println!("✓ Already in a git repository - skipping git initialization");
    } else {
        // Try to initialize git repository
        let git_result = run_with_spinner_async(
            "🔧 Initializing git repository...",
            "✅ Git repository initialized",
            || async {
                let mut init_cmd = Command::new("git");
                init_cmd.arg("init").current_dir(&app_path);
                run_command(&mut init_cmd, "Failed to initialize git repository").await?;

                let mut add_cmd = Command::new("git");
                add_cmd.arg("add").arg(".").current_dir(&app_path);
                run_command(&mut add_cmd, "Failed to add files to git repository").await?;

                let mut commit_cmd = Command::new("git");
                commit_cmd
                    .arg("commit")
                    .arg("-m")
                    .arg("init")
                    .current_dir(&app_path);
                run_command(&mut commit_cmd, "Failed to commit files to git repository").await?;
                Ok(())
            },
        )
        .await;

        // If git initialization failed, warn but continue
        if let Err(err) = git_result {
            println!("⚠️  Git initialization failed: {err}");
            println!("   Continuing with project setup...");
        }
    }

    // Configure apx in pyproject.toml (dependencies will be installed on first command)
    let pyproject_path = app_path.join("pyproject.toml");
    let apx_version = env!("CARGO_PKG_VERSION");

    if let Ok(apx_dev_path) = std::env::var("APX_DEV_PATH") {
        // Editable mode: configure path-based source
        let apx_path = PathBuf::from(&apx_dev_path);
        if !apx_path.is_dir() {
            return Err(format!(
                "APX_DEV_PATH is not a valid directory: {apx_dev_path}"
            ));
        }
        configure_editable_apx(&pyproject_path, &apx_path)?;
        debug!("Configured editable apx from APX_DEV_PATH");
    } else {
        // Standard mode: configure index-based source with version
        ensure_apx_uv_config(&pyproject_path, apx_version)?;
        debug!("Configured apx {} from index", apx_version);
    }

    if let Some(assistant) = args.assistant.take() {
        let rules_dir = templates_dir.join("addons");
        run_with_spinner(
            "🤖 Setting up assistant rules...",
            "✅ Assistant rules configured",
            || {
                match assistant {
                    Assistant::Vscode => process_template_directory(
                        &rules_dir.join("vscode"),
                        &app_path,
                        &app_name,
                        &app_slug,
                    )?,
                    Assistant::Cursor => process_template_directory(
                        &rules_dir.join("cursor"),
                        &app_path,
                        &app_name,
                        &app_slug,
                    )?,
                    Assistant::Claude => process_template_directory(
                        &rules_dir.join("claude"),
                        &app_path,
                        &app_name,
                        &app_slug,
                    )?,
                    Assistant::Codex => {
                        process_template_directory(
                            &rules_dir.join("codex"),
                            &app_path,
                            &app_name,
                            &app_slug,
                        )?;
                        println!("Please note that Codex mcp config is not supported yet.");
                        println!(
                            "Follow this guide to set it up manually: https://ui.shadcn.com/docs/mcp#codex"
                        );
                    }
                }
                Ok(())
            },
        )?;
    }

    // Add shadcn components for non-minimal templates
    if !matches!(template, Template::Minimal) {
        let mut components_to_add = vec![ComponentInput::new("button")];

        if matches!(layout, Layout::Sidebar) {
            components_to_add.extend([
                ComponentInput::new("avatar"),
                ComponentInput::new("sidebar"),
                ComponentInput::new("separator"),
                ComponentInput::new("skeleton"),
                ComponentInput::new("badge"),
                ComponentInput::new("card"),
            ]);
        }

        let components_start = Instant::now();
        let sp = spinner("🎨 Adding components...");

        let result = add_components(&app_path, &components_to_add, true).await?;

        sp.finish_and_clear();
        println!(
            "✅ Components added ({})",
            format_elapsed_ms(components_start)
        );

        if !result.warnings.is_empty() {
            for warning in &result.warnings {
                eprintln!("   ⚠️  {warning}");
            }
        }
    }

    println!();
    println!("✨ Project {app_name} initialized successfully!");
    let canonical_path = app_path.canonicalize().unwrap_or_else(|_| app_path.clone());
    println!(
        "🚀 Run `cd {} && apx dev start` to get started!",
        canonical_path.display()
    );
    println!("   (Dependencies will be installed automatically on first run)");
    Ok(())
}

fn normalize_app_name(app_name: &str) -> Result<String, String> {
    let normalized = app_name.to_lowercase().replace([' ', '_'], "-");
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(
            "Invalid app name. Please use only alphanumeric characters and dashes.".to_string(),
        );
    }
    Ok(normalized)
}

fn random_name() -> String {
    let adjectives = [
        "fast",
        "simple",
        "clean",
        "elegant",
        "modern",
        "cool",
        "awesome",
        "brave",
        "bold",
        "creative",
        "curious",
        "dynamic",
        "energetic",
        "fantastic",
        "giant",
    ];
    let animals = [
        "lion", "tiger", "bear", "wolf", "fox", "dog", "cat", "bird", "fish", "horse", "rabbit",
        "turtle", "whale", "dolphin", "shark", "octopus",
    ];
    let mut rng = rand::thread_rng();
    let adj = adjectives.choose(&mut rng).unwrap_or(&"fast");
    let animal = animals.choose(&mut rng).unwrap_or(&"lion");
    format!("{adj}-{animal}")
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("Failed to create directory: {err}"))
}

fn process_template_directory(
    source_dir: &Path,
    target_dir: &Path,
    app_name: &str,
    app_slug: &str,
) -> Result<(), String> {
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
        if path_str.contains("/base/") || path_str.starts_with("base/") {
            path_str = path_str
                .replace("/base/", &format!("/{app_slug}/"))
                .replace("base/", &format!("{app_slug}/"));
        }

        let is_template = entry.path().extension() == Some(OsStr::new("jinja2"));
        let target_path = if is_template {
            let trimmed = path_str.trim_end_matches(".jinja2");
            target_dir.join(trimmed)
        } else {
            target_dir.join(&path_str)
        };

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create directory: {err}"))?;
        }

        if is_template {
            let content = fs::read_to_string(entry.path())
                .map_err(|err| format!("Failed to read template: {err}"))?;
            let mut context = Context::new();
            context.insert("app_name", app_name);
            context.insert("app_slug", app_slug);
            context.insert(
                "app_letter",
                &app_name.chars().next().unwrap_or('A').to_string(),
            );
            let rendered = tera::Tera::one_off(&content, &context, false).map_err(|err| {
                format!(
                    "File {} in template is not tera compatible. File content: {content}\nError: {err}",
                    entry.path().display()
                )
            })?;
            fs::write(&target_path, rendered)
                .map_err(|err| format!("Failed to write template output: {err}"))?;
        } else {
            fs::copy(entry.path(), &target_path)
                .map_err(|err| format!("Failed to copy template file: {err}"))?;
        }
    }
    Ok(())
}

async fn is_in_git_repo(path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .output()
        .await
        .map_err(|err| format!("Failed to check git repository: {err}"))?;
    let is_inside =
        output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true";
    if is_inside {
        return Ok(true);
    }
    Ok(has_git_dir(path))
}

fn has_git_dir(path: &Path) -> bool {
    for ancestor in path.ancestors() {
        let candidate = ancestor.join(".git");
        if candidate.is_dir() {
            return true;
        }
    }
    false
}

async fn run_command(cmd: &mut Command, error_msg: &str) -> Result<(), String> {
    let output = cmd
        .output()
        .await
        .map_err(|err| format!("{error_msg}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut message = format!("❌ {error_msg}");
    if !stderr.trim().is_empty() {
        message.push_str(&format!("\n{stderr}"));
    }
    if !stdout.trim().is_empty() {
        message.push_str(&format!("\n{stdout}"));
    }
    Err(message)
}

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

/// Configure pyproject.toml for standard apx installation from index.
/// Adds [[tool.uv.index]] for apx-index, [tool.uv.sources].apx pointing to that index,
/// and "apx=={version}" to [dependency-groups].dev.
pub fn ensure_apx_uv_config(pyproject: &Path, version: &str) -> Result<(), String> {
    let contents =
        fs::read_to_string(pyproject).map_err(|e| format!("Failed to read pyproject.toml: {e}"))?;

    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Invalid TOML: {e}"))?;

    // --- [[tool.uv.index]] ---
    let tool = doc["tool"].or_insert(Item::Table(Table::new()));
    let uv = tool["uv"].or_insert(Item::Table(Table::new()));

    let indexes = uv["index"].or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let indexes = indexes
        .as_array_of_tables_mut()
        .ok_or("tool.uv.index is not an array")?;

    let exists = indexes
        .iter()
        .any(|tbl| tbl.get("name").and_then(|v| v.as_str()) == Some("apx-index"));

    if !exists {
        let mut tbl = Table::new();
        tbl["name"] = "apx-index".into();
        tbl["url"] = APX_INDEX_URL.into();
        indexes.push(tbl);
    }

    // --- [tool.uv.sources] ---
    let sources = uv["sources"].or_insert(Item::Table(Table::new()));
    let sources = sources
        .as_table_mut()
        .ok_or("tool.uv.sources is not a table")?;

    if !sources.contains_key("apx") {
        let mut apx = Table::new();
        apx["index"] = "apx-index".into();
        sources["apx"] = Item::Table(apx);
    }

    // --- [dependency-groups].dev ---
    let dep_groups = doc["dependency-groups"].or_insert(Item::Table(Table::new()));
    let dep_groups = dep_groups
        .as_table_mut()
        .ok_or("dependency-groups is not a table")?;

    let dev_deps = dep_groups["dev"].or_insert(Item::Value(Value::Array(Array::new())));
    let dev_array = dev_deps
        .as_array_mut()
        .ok_or("dependency-groups.dev is not an array")?;

    // Add "apx=={version}" if not present (check for any apx entry)
    let apx_exists = dev_array
        .iter()
        .any(|v| v.as_str().map(|s| s.starts_with("apx")).unwrap_or(false));
    if !apx_exists {
        let apx_spec = format!("apx=={version}");
        dev_array.push(apx_spec.as_str());
    }

    fs::write(pyproject, doc.to_string())
        .map_err(|e| format!("Failed to write pyproject.toml: {e}"))?;

    Ok(())
}

/// Configure pyproject.toml for editable apx installation.
/// Adds apx to [tool.uv.sources] with path and editable=true,
/// and appends "apx" to [dependency-groups].dev list.
pub fn configure_editable_apx(pyproject: &Path, apx_path: &Path) -> Result<(), String> {
    debug!("Configuring editable apx in pyproject.toml");
    debug!("  pyproject path: {}", pyproject.display());
    debug!("  apx path: {}", apx_path.display());

    let contents =
        fs::read_to_string(pyproject).map_err(|e| format!("Failed to read pyproject.toml: {e}"))?;

    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Invalid TOML: {e}"))?;

    // --- [tool.uv.sources] ---
    let tool = doc["tool"].or_insert(Item::Table(Table::new()));
    let uv = tool["uv"].or_insert(Item::Table(Table::new()));
    let sources = uv["sources"].or_insert(Item::Table(Table::new()));
    let sources = sources
        .as_table_mut()
        .ok_or("tool.uv.sources is not a table")?;

    // Add apx = { path = "...", editable = true }
    let apx_path_str = apx_path.to_string_lossy().to_string();
    debug!("  Setting apx source path to: {}", apx_path_str);

    let mut apx_source = InlineTable::new();
    apx_source.insert("path", Value::from(apx_path_str.as_str()));
    apx_source.insert("editable", Value::from(true));
    sources["apx"] = Item::Value(Value::InlineTable(apx_source));

    // --- [dependency-groups].dev ---
    let dep_groups = doc["dependency-groups"].or_insert(Item::Table(Table::new()));
    let dep_groups = dep_groups
        .as_table_mut()
        .ok_or("dependency-groups is not a table")?;

    let dev_deps = dep_groups["dev"].or_insert(Item::Value(Value::Array(Array::new())));
    let dev_array = dev_deps
        .as_array_mut()
        .ok_or("dependency-groups.dev is not an array")?;

    // Check if "apx" already exists in dev dependencies
    let apx_exists = dev_array.iter().any(|v| v.as_str() == Some("apx"));
    if !apx_exists {
        debug!("  Adding 'apx' to dependency-groups.dev");
        dev_array.push("apx");
    } else {
        debug!("  'apx' already in dependency-groups.dev");
    }

    let output = doc.to_string();
    debug!("  Writing updated pyproject.toml");
    fs::write(pyproject, output).map_err(|e| format!("Failed to write pyproject.toml: {e}"))?;

    debug!("  Editable apx configuration complete");
    Ok(())
}

async fn is_command_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal pyproject.toml for testing
    fn create_test_pyproject(dir: &Path) -> PathBuf {
        let pyproject_path = dir.join("pyproject.toml");
        let content = r#"[project]
name = "test-app"
version = "0.1.0"

[dependency-groups]
dev = [
    "pytest",
]
"#;
        fs::write(&pyproject_path, content).unwrap();
        pyproject_path
    }

    #[test]
    fn test_ensure_apx_uv_config_adds_index_and_sources() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(temp_dir.path());

        // Run the function
        ensure_apx_uv_config(&pyproject_path, "0.1.27").unwrap();

        // Read and parse the result
        let content = fs::read_to_string(&pyproject_path).unwrap();

        // Verify index was added
        assert!(content.contains("[[tool.uv.index]]"));
        assert!(content.contains("name = \"apx-index\""));
        assert!(content.contains("https://databricks-solutions.github.io/apx/simple"));

        // Verify sources was added
        assert!(content.contains("[tool.uv.sources]"));
        assert!(content.contains("[tool.uv.sources.apx]"));
        assert!(content.contains("index = \"apx-index\""));

        // Verify dev dependency was added
        assert!(content.contains("apx==0.1.27"));
    }

    #[test]
    fn test_ensure_apx_uv_config_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(temp_dir.path());

        // Run twice
        ensure_apx_uv_config(&pyproject_path, "0.1.27").unwrap();
        ensure_apx_uv_config(&pyproject_path, "0.1.28").unwrap(); // different version

        // Read and parse the result
        let content = fs::read_to_string(&pyproject_path).unwrap();

        // Should only have one apx-index entry
        assert_eq!(content.matches("name = \"apx-index\"").count(), 1);

        // Should only have one apx dependency (the first one)
        assert!(content.contains("apx==0.1.27"));
        assert!(!content.contains("apx==0.1.28"));
    }

    #[test]
    fn test_configure_editable_apx() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(temp_dir.path());

        // Create a fake apx path
        let apx_path = temp_dir.path().join("apx-dev");
        fs::create_dir(&apx_path).unwrap();

        // Run the function
        configure_editable_apx(&pyproject_path, &apx_path).unwrap();

        // Read and parse the result
        let content = fs::read_to_string(&pyproject_path).unwrap();

        // Verify editable source was added
        assert!(content.contains("[tool.uv.sources]"));
        assert!(content.contains("editable = true"));
        assert!(content.contains(&apx_path.to_string_lossy().to_string()));

        // Verify dev dependency was added (just "apx", not versioned)
        let doc: DocumentMut = content.parse().unwrap();
        let dev_deps = doc["dependency-groups"]["dev"].as_array().unwrap();
        let has_apx = dev_deps.iter().any(|v| v.as_str() == Some("apx"));
        assert!(has_apx, "Expected 'apx' in dev dependencies");
    }

    #[test]
    fn test_configure_editable_apx_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_path = create_test_pyproject(temp_dir.path());

        let apx_path = temp_dir.path().join("apx-dev");
        fs::create_dir(&apx_path).unwrap();

        // Run twice
        configure_editable_apx(&pyproject_path, &apx_path).unwrap();
        configure_editable_apx(&pyproject_path, &apx_path).unwrap();

        // Read and parse the result
        let content = fs::read_to_string(&pyproject_path).unwrap();
        let doc: DocumentMut = content.parse().unwrap();

        // Should only have one apx in dev dependencies
        let dev_deps = doc["dependency-groups"]["dev"].as_array().unwrap();
        let apx_count = dev_deps
            .iter()
            .filter(|v| v.as_str() == Some("apx"))
            .count();
        assert_eq!(
            apx_count, 1,
            "Expected exactly one 'apx' in dev dependencies"
        );
    }

    #[test]
    fn test_normalize_app_name_basic() {
        assert_eq!(normalize_app_name("my-app").unwrap(), "my-app");
        assert_eq!(normalize_app_name("MyApp").unwrap(), "myapp");
        assert_eq!(normalize_app_name("my_app").unwrap(), "my-app");
        assert_eq!(normalize_app_name("my app").unwrap(), "my-app");
    }

    #[test]
    fn test_normalize_app_name_invalid() {
        assert!(normalize_app_name("my@app").is_err());
        assert!(normalize_app_name("my/app").is_err());
        assert!(normalize_app_name("my.app").is_err());
    }
}
