use clap::Args;

use crate::run_cli_async_helper;

#[derive(Args, Debug, Clone)]
pub struct FeedbackArgs {
    /// Feedback message
    #[arg(value_name = "MESSAGE")]
    pub message: Option<String>,

    /// Title for the feedback issue
    #[arg(long, short = 't')]
    pub title: Option<String>,

    /// Category: docs, bug, feature, skill, general
    #[arg(long, short = 'c')]
    pub category: Option<String>,

    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Exclude auto-collected metadata (version, OS, arch)
    #[arg(long)]
    pub no_metadata: bool,

    /// Preview the issue without submitting
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: FeedbackArgs) -> i32 {
    run_cli_async_helper(|| run_inner(args)).await
}

async fn run_inner(args: FeedbackArgs) -> Result<(), String> {
    let message = match args.message {
        Some(m) => m,
        None => dialoguer::Input::<String>::new()
            .with_prompt("Feedback message")
            .interact_text()
            .map_err(|e| format!("Failed to read input: {e}"))?,
    };

    if message.trim().is_empty() {
        return Err("Feedback message cannot be empty".to_string());
    }

    // Prepare the issue for preview
    let prepared = apx_core::feedback::prepare_feedback(
        args.title.as_deref(),
        &message,
        args.category.as_deref(),
        !args.no_metadata,
    );

    // Always show preview
    println!("\n\x1b[1m--- Issue Preview ---\x1b[0m");
    println!("\x1b[1mTitle:\x1b[0m {}\n", prepared.title);
    println!("{}", prepared.body);
    println!("\x1b[1m--- End Preview ---\x1b[0m\n");

    if args.dry_run {
        println!("Browser URL:\n  {}", prepared.browser_url);
        return Ok(());
    }

    // Confirm before submitting (issue will be public)
    if !args.yes {
        let confirmed = dialoguer::Confirm::new()
            .with_prompt("This will create a public GitHub issue. Continue?")
            .default(true)
            .interact()
            .map_err(|e| format!("Failed to read confirmation: {e}"))?;

        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    let sp = apx_core::common::spinner("Submitting feedback...");
    let result = apx_core::feedback::submit_prepared(&prepared).await;
    sp.finish_and_clear();

    match result {
        apx_core::feedback::FeedbackResult::Submitted { url } => {
            println!("\x1b[32m✓\x1b[0m Feedback submitted: {url}");
        }
        apx_core::feedback::FeedbackResult::Fallback { url, .. } => {
            println!(
                "\x1b[33m⚠\x1b[0m Could not submit automatically (gh CLI not found or not authenticated).\n"
            );
            println!("Open this link to submit manually:\n  {url}");
        }
    }

    Ok(())
}
