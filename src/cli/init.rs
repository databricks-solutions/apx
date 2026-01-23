use clap::{Args, ValueEnum};
use dialoguer::{Confirm, Input, Select};
use indicatif::{ProgressBar, ProgressStyle};
use rand::seq::SliceRandom;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tera::Context;
use tokio::process::Command;
use walkdir::WalkDir;

use crate::bun_binary_path;
use crate::cli::run_cli_async;
use crate::cli::components::add::run_inner as add_component;
use crate::cli::components::add::ComponentsAddArgs;
use crate::common::bun_install;
use crate::dotenv::DotenvFile;
use crate::interop::{list_profiles, templates_dir};

const DEFAULT_APX_PACKAGE: &str = "https://github.com/databricks-solutions/apx.git";

#[derive(ValueEnum, Clone, Debug)]
#[value(rename_all = "lower")]
pub enum Template {
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
    #[arg(
        long = "apx-package",
        default_value = DEFAULT_APX_PACKAGE,
        hide = true,
        help = "The apx package to install. Used for internal testing and development."
    )]
    pub apx_package: String,
    #[arg(
        long = "apx-editable",
        hide = true,
        help = "Whether to install apx as editable package."
    )]
    pub apx_editable: bool,
    #[arg(
        long = "skip-frontend-dependencies",
        help = "Skip installing frontend dependencies (bun packages)."
    )]
    pub skip_frontend_dependencies: bool,
    #[arg(
        long = "skip-backend-dependencies",
        help = "Skip installing backend dependencies (uv sync)."
    )]
    pub skip_backend_dependencies: bool,
    #[arg(
        long = "skip-build",
        help = "Skip building the project after initialization."
    )]
    pub skip_build: bool,
}

pub async fn run(args: InitArgs) -> i32 {
    run_cli_async(|| run_inner(args)).await
}

async fn run_inner(mut args: InitArgs) -> Result<(), String> {
    if !is_command_available("uv").await {
        return Err("uv is not installed. Please install uv to continue.".to_string());
    }

    let bun_path = bun_binary_path()?;
    if !bun_path.exists() {
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
        let choices = vec![Template::Essential, Template::Stateful];
        let default_idx = 0;
        let selection = Select::new()
            .with_prompt("Which template would you like to use?")
            .items(&["essential", "stateful"])
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
            let choices = vec![
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

    if args.layout.is_none() {
        let choices = vec![Layout::Sidebar, Layout::Basic];
        let selection = Select::new()
            .with_prompt("Which layout would you like to use?")
            .items(&["sidebar", "basic"])
            .default(0)
            .interact()
            .map_err(|err| format!("Failed to select layout: {err}"))?;
        args.layout = Some(choices[selection].clone());
    }

    let template = args.template.take().unwrap_or(Template::Essential);
    let layout = args.layout.take().unwrap_or(Layout::Sidebar);

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
            println!("⚠️  Git initialization failed: {}", err);
            println!("   Continuing with project setup...");
        }
    }

    let backend_task = if !args.skip_backend_dependencies {
        let app_path = app_path.clone();
        let apx_package = args.apx_package.clone();
        let apx_editable = args.apx_editable;
        Some(tokio::spawn(async move {
            setup_backend(&app_path, &apx_package, apx_editable).await
        }))
    } else {
        None
    };

    let frontend_task = if !args.skip_frontend_dependencies {
        let app_path = app_path.clone();
        let bun_path = bun_path.clone();
        Some(tokio::spawn(
            async move { bun_install(&app_path, &bun_path).await },
        ))
    } else {
        None
    };

    if backend_task.is_some() || frontend_task.is_some() {
        let spinner = spinner("📦 Installing dependencies...");
        let dependencies_start = Instant::now();

        let backend_result = if let Some(handle) = backend_task {
            Some(
                handle
                    .await
                    .unwrap_or_else(|_| Err("Backend setup panicked".to_string())),
            )
        } else {
            None
        };
        let frontend_result = if let Some(handle) = frontend_task {
            Some(
                handle
                    .await
                    .unwrap_or_else(|_| Err("Frontend setup panicked".to_string())),
            )
        } else {
            None
        };

        spinner.finish_and_clear();

        if let Some(Err(err)) = frontend_result {
            return Err(err);
        }
        if let Some(Err(err)) = backend_result {
            return Err(err);
        }

        println!(
            "✅ Dependencies installed ({})",
            format_elapsed_ms(dependencies_start)
        );
    }

    if !args.skip_frontend_dependencies {
        run_with_spinner_async(
            "🎨 Bootstrapping shadcn components...",
            "✅ Shadcn components added",
            || async {
                add_component(ComponentsAddArgs {
                    component: "button".to_string(),
                    registry: None,
                    force: true,
                    dry_run: false,
                    app_path: Some(app_path.clone()),
                })
                .await?;
                
                if matches!(layout, Layout::Sidebar) {
                    let components = vec![
                        "avatar",
                        "sidebar",
                        "separator",
                        "skeleton",
                        "badge",
                        "card",
                    ];
                    for comp in components {
                        add_component(ComponentsAddArgs {
                            component: comp.to_string(),
                            registry: None,
                            force: true,
                            dry_run: false,
                            app_path: Some(app_path.clone()),
                        })
                        .await?;
                    }
                }
                Ok(())
            },
        )
        .await?;
    }

    if !args.skip_build {
        run_with_spinner_async("🔧 Building project...", "✅ Project built", || async {
            let mut cmd = Command::new("uv");
            cmd.arg("run")
                .arg("apx")
                .arg("build")
                .current_dir(&app_path);
            run_command(&mut cmd, "Failed to build project").await
        })
        .await?;
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

    println!();
    println!("✨ Project {} initialized successfully!", app_name);
    println!(
        "🚀 Run `cd {} && uv run apx dev start` to get started!",
        app_path
            .canonicalize()
            .unwrap_or_else(|_| app_path.clone())
            .display()
    );
    Ok(())
}

fn run_with_spinner<F>(description: &str, success_message: &str, f: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let spinner = spinner(description);
    let start = Instant::now();
    let result = f();
    spinner.finish_and_clear();
    if result.is_ok() {
        println!("{} ({})", success_message, format_elapsed_ms(start));
    }
    result
}

async fn run_with_spinner_async<F, Fut>(
    description: &str,
    success_message: &str,
    f: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let spinner = spinner(description);
    let start = Instant::now();
    let result = f().await;
    spinner.finish_and_clear();
    if result.is_ok() {
        println!("{} ({})", success_message, format_elapsed_ms(start));
    }
    result
}

fn spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(message.to_string());
    spinner
}

fn format_elapsed_ms(start: Instant) -> String {
    let elapsed = start.elapsed();
    if elapsed.as_secs() == 0 {
        return format!("{}ms", elapsed.as_millis());
    }
    let seconds = elapsed.as_secs();
    let remaining_ms = elapsed.subsec_millis();
    format!("{seconds}s {remaining_ms}ms")
}

fn normalize_app_name(app_name: &str) -> Result<String, String> {
    let normalized = app_name.to_lowercase().replace(' ', "-").replace('_', "-");
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
    let is_inside = output.status.success()
        && String::from_utf8_lossy(&output.stdout).trim() == "true";
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


async fn setup_backend(app_path: &Path, apx_package: &str, apx_editable: bool) -> Result<(), String> {
    generate_metadata_file(app_path)?;
    if !apx_package.is_empty() {
        let mut cmd = Command::new("uv");
        cmd.arg("add").arg("--dev");
        if apx_editable {
            cmd.arg("--editable");
        }
        cmd.arg(apx_package).current_dir(app_path);
        run_command(&mut cmd, "Failed to add apx package").await?;
    }
    let mut sync_cmd = Command::new("uv");
    sync_cmd.arg("sync").current_dir(app_path);
    run_command(&mut sync_cmd, "Failed to set up project").await
}

fn generate_metadata_file(app_path: &Path) -> Result<(), String> {
    let pyproject_path = app_path.join("pyproject.toml");
    let contents = fs::read_to_string(&pyproject_path).map_err(|err| format!("{err}"))?;
    let value: toml::Value = contents
        .parse()
        .map_err(|err| format!("Failed to parse pyproject.toml: {err}"))?;
    let metadata = value
        .get("tool")
        .and_then(|tool| tool.get("apx"))
        .and_then(|apx| apx.get("metadata"))
        .ok_or_else(|| "Missing tool.apx.metadata in pyproject.toml".to_string())?;

    let app_name = metadata
        .get("app-name")
        .and_then(|val| val.as_str())
        .ok_or_else(|| "Missing app-name in pyproject.toml metadata".to_string())?;
    let app_module = metadata
        .get("app-module")
        .and_then(|val| val.as_str())
        .ok_or_else(|| "Missing app-module in pyproject.toml metadata".to_string())?;
    let app_slug = metadata
        .get("app-slug")
        .and_then(|val| val.as_str())
        .ok_or_else(|| "Missing app-slug in pyproject.toml metadata".to_string())?;
    let api_prefix = metadata
        .get("api-prefix")
        .and_then(|val| val.as_str())
        .ok_or_else(|| "Missing api-prefix in pyproject.toml metadata".to_string())?;
    let metadata_path = metadata
        .get("metadata-path")
        .and_then(|val| val.as_str())
        .ok_or_else(|| "Missing metadata-path in pyproject.toml metadata".to_string())?;

    let target_path = app_path.join(metadata_path);
    let contents = [
        format!("app_name = \"{app_name}\""),
        format!("app_module = \"{app_module}\""),
        format!("app_slug = \"{app_slug}\""),
        format!("api_prefix = \"{api_prefix}\""),
    ]
    .join("\n");
    fs::write(target_path, contents).map_err(|err| format!("Failed to write metadata file: {err}"))
}

async fn is_command_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

