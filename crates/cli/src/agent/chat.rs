//! `apx agent chat` — interactive chat with a Databricks-hosted model.

use std::path::PathBuf;

use apx_agent::{AgentClient, SessionStore, SqliteSessionStore};
use apx_common::EnvProfile;
use clap::Args;
use dialoguer::{Select, theme::ColorfulTheme};

use crate::run_cli_async_helper;

/// Arguments for the `agent chat` command.
#[derive(Args)]
pub struct ChatArgs {
    /// Path to the app directory (used for .env profile resolution).
    #[arg(value_name = "APP_PATH")]
    pub app_path: Option<PathBuf>,

    /// Databricks CLI profile name.
    #[arg(short = 'p', long = "profile")]
    pub profile: Option<String>,

    /// Serving endpoint name to use as the model.
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,
}

/// Entry point for `apx agent chat`.
pub async fn run(args: ChatArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

/// Resolve the Databricks profile from flag, env, .env, or default.
fn resolve_profile(args: &ChatArgs) -> String {
    let dotenv_vars = args
        .app_path
        .as_ref()
        .and_then(|dir| apx_core::dotenv::DotenvFile::read(&dir.join(".env")).ok())
        .map(|d| d.get_vars())
        .unwrap_or_default();
    EnvProfile::new(&dotenv_vars).retrieve(args.profile.as_deref())
}

async fn run_inner(args: ChatArgs) -> Result<(), String> {
    let profile = resolve_profile(&args);
    let client = AgentClient::from_profile(&profile)
        .await
        .map_err(|e| format!("Failed to create agent client: {e}"))?;

    let models = client
        .list_models()
        .await
        .map_err(|e| format!("Failed to list models: {e}"))?;

    if models.is_empty() {
        return Err("No chat-capable models found in the workspace".into());
    }

    let model_name = select_model(&args, &models)?;

    let store = SqliteSessionStore::open()
        .await
        .map_err(|e| format!("Failed to open session store: {e}"))?;

    let session = store
        .create_session(&model_name)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

    super::tui::run(client, store, session, &model_name).await
}

/// Select a model from the list or validate the explicit flag.
fn select_model(args: &ChatArgs, models: &[apx_agent::ModelRef]) -> Result<String, String> {
    if let Some(name) = &args.model {
        if models.iter().any(|m| m.name == *name) {
            return Ok(name.clone());
        }
        return Err(format!(
            "Model '{name}' not found. Available: {}",
            models
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let items: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a model")
        .items(&items)
        .default(0)
        .interact()
        .map_err(|e| format!("Model selection failed: {e}"))?;

    Ok(models[selection].name.clone())
}
