//! `/model` command — switch the active model mid-session.

use apx_agent::CommandArgs;
use dialoguer::{Select, theme::ColorfulTheme};

use super::{CommandContext, CommandOutcome};

/// Run the `/model` command: list models and let the user pick one.
///
/// Returns [`CommandOutcome::ModelChanged`] with the chosen name; the caller
/// is responsible for persisting the change and updating app state.
pub async fn run(args: &CommandArgs, ctx: CommandContext<'_>) -> CommandOutcome {
    if let Some(explicit) = args.get(0) {
        return CommandOutcome::ModelChanged(explicit.to_string());
    }

    let models = match ctx.client.list_models().await {
        Ok(m) => m,
        Err(e) => return CommandOutcome::CommandError(format!("Failed to list models: {e}")),
    };

    if models.is_empty() {
        return CommandOutcome::CommandError("No chat-capable models found".into());
    }

    let items: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
    let selection = match Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a model")
        .items(&items)
        .default(0)
        .interact()
    {
        Ok(i) => i,
        Err(e) => return CommandOutcome::CommandError(format!("Selection cancelled: {e}")),
    };

    CommandOutcome::ModelChanged(models[selection].name.clone())
}
