//! Ratatui-based TUI for interactive chat.

use std::time::Duration;

use apx_agent::{
    AgentClient, ChatEvent, ChatMessage, ParsedInput, Role, Session, SessionStore,
    SqliteSessionStore, now_secs, parse_input,
};

use super::commands::{self, CommandContext, CommandOutcome};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::mpsc;

/// The kind of message shown in the chat area.
enum MessageKind {
    Chat(Role),
    Info,
}

/// A message displayed in the chat area.
struct DisplayMessage {
    kind: MessageKind,
    content: String,
}

/// Application state for the TUI.
struct App {
    messages: Vec<DisplayMessage>,
    input: String,
    scroll_offset: u16,
    streaming: bool,
    should_quit: bool,
    needs_reinit: bool,
    model_name: String,
    session: Session,
}

impl App {
    fn new(session: Session, model_name: &str) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            scroll_offset: 0,
            streaming: false,
            should_quit: false,
            needs_reinit: false,
            model_name: model_name.into(),
            session,
        }
    }
}

/// Launch the TUI event loop.
///
/// # Errors
///
/// Returns an error if terminal setup fails or a fatal I/O error occurs.
pub async fn run(
    client: AgentClient,
    store: SqliteSessionStore,
    session: Session,
    model_name: &str,
) -> Result<(), String> {
    let mut terminal = ratatui::init();

    // Install a panic hook that restores the terminal before printing the panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let result = run_event_loop(&mut terminal, client, store, session, model_name).await;

    ratatui::restore();
    result
}

/// Core event loop: reads keys + chat events + tick timer.
async fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    client: AgentClient,
    store: SqliteSessionStore,
    session: Session,
    model_name: &str,
) -> Result<(), String> {
    let mut app = App::new(session, model_name);
    let (chat_tx, mut chat_rx) = mpsc::channel::<ChatEvent>(64);
    let mut event_stream = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(200));

    loop {
        terminal
            .draw(|f| draw(f, &app))
            .map_err(|e| format!("draw error: {e}"))?;

        tokio::select! {
            // Terminal events (keyboard input)
            maybe_event = event_stream.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    handle_key(&mut app, key, &client, &store, &chat_tx).await?;
                }
            }
            // Chat stream events
            Some(event) = chat_rx.recv() => {
                handle_chat_event(&mut app, &store, event).await?;
            }
            // Tick for cursor blink / redraw
            _ = tick.tick() => {}
        }

        // Re-enter TUI after a command that suspended raw mode (e.g. /model).
        if app.needs_reinit {
            *terminal = ratatui::init();
            app.needs_reinit = false;
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Handle a keyboard event.
async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    client: &AgentClient,
    store: &SqliteSessionStore,
    chat_tx: &mpsc::Sender<ChatEvent>,
) -> Result<(), String> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Enter, _) if !app.streaming && !app.input.is_empty() => {
            handle_enter(app, client, store, chat_tx).await?;
        }
        (KeyCode::Char(c), _) if !app.streaming => {
            app.input.push(c);
        }
        (KeyCode::Backspace, _) if !app.streaming => {
            app.input.pop();
        }
        (KeyCode::Up, _) => {
            app.scroll_offset = app.scroll_offset.saturating_add(1);
        }
        (KeyCode::Down, _) => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
        }
        _ => {}
    }
    Ok(())
}

/// Route user input to either the chat stream or the command dispatcher.
async fn handle_enter(
    app: &mut App,
    client: &AgentClient,
    store: &SqliteSessionStore,
    chat_tx: &mpsc::Sender<ChatEvent>,
) -> Result<(), String> {
    match parse_input(&app.input) {
        ParsedInput::Message(_) => send_message(app, client, store, chat_tx).await,
        ParsedInput::Command { name, args } => {
            app.input.clear();
            handle_command(app, &name, &args, client, store).await;
            Ok(())
        }
    }
}

/// Send the current input as a user message and start streaming the response.
async fn send_message(
    app: &mut App,
    client: &AgentClient,
    store: &SqliteSessionStore,
    chat_tx: &mpsc::Sender<ChatEvent>,
) -> Result<(), String> {
    let content = std::mem::take(&mut app.input);
    let now = now_secs();

    // Save and display user message
    let user_msg = ChatMessage {
        role: Role::User,
        content: content.clone(),
        timestamp: now,
    };
    store
        .append_message(&app.session.id, &user_msg)
        .await
        .map_err(|e| format!("save message: {e}"))?;
    app.messages.push(DisplayMessage {
        kind: MessageKind::Chat(Role::User),
        content,
    });

    // Add empty assistant placeholder
    app.messages.push(DisplayMessage {
        kind: MessageKind::Chat(Role::Assistant),
        content: String::new(),
    });
    app.streaming = true;
    app.scroll_offset = 0;

    // Build history from stored messages for context
    let history = store
        .load_messages(&app.session.id)
        .await
        .map_err(|e| format!("load history: {e}"))?;

    // Spawn background streaming task
    let client = client.clone();
    let model = app.model_name.clone();
    let message = user_msg.content.clone();
    let tx = chat_tx.clone();
    tokio::spawn(async move {
        match client.stream_chat(&model, &message, &history).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(event) => {
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            // Send the error as a Done with error text
                            let _ = tx.send(ChatEvent::Done(format!("[Error: {e}]"))).await;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(ChatEvent::Done(format!("[Error: {e}]"))).await;
            }
        }
    });

    Ok(())
}

/// Handle a `ChatEvent` from the streaming task.
async fn handle_chat_event(
    app: &mut App,
    store: &SqliteSessionStore,
    event: ChatEvent,
) -> Result<(), String> {
    match event {
        ChatEvent::Token(text) => {
            if let Some(last) = app.messages.last_mut() {
                last.content.push_str(&text);
            }
        }
        ChatEvent::Done(full_text) => {
            // Ensure the display message has the full text
            if let Some(last) = app.messages.last_mut() {
                last.content.clone_from(&full_text);
            }
            app.streaming = false;

            // Persist assistant message
            let asst_msg = ChatMessage {
                role: Role::Assistant,
                content: full_text,
                timestamp: now_secs(),
            };
            store
                .append_message(&app.session.id, &asst_msg)
                .await
                .map_err(|e| format!("save assistant message: {e}"))?;
        }
    }
    Ok(())
}

/// Handle a parsed slash command.
///
/// Commands that need interactive prompts (e.g. `/model`) suspend raw mode
/// so that `dialoguer` can function, then signal the event loop to re-init
/// the terminal.
async fn handle_command(
    app: &mut App,
    name: &apx_agent::CommandName,
    args: &apx_agent::CommandArgs,
    client: &AgentClient,
    store: &SqliteSessionStore,
) {
    let suspended = commands::needs_terminal_suspend(name);
    if suspended {
        ratatui::restore();
    }

    let ctx = CommandContext { client };
    let outcome = commands::dispatch(name, args, ctx).await;
    apply_outcome(app, outcome, store).await;

    app.needs_reinit = suspended;
}

/// Apply a [`CommandOutcome`] to application state.
///
/// All app mutations from command results happen here — handlers stay pure.
async fn apply_outcome(app: &mut App, outcome: CommandOutcome, store: &SqliteSessionStore) {
    match outcome {
        CommandOutcome::Quit => {
            app.should_quit = true;
        }
        CommandOutcome::ModelChanged(name) => {
            app.model_name.clone_from(&name);
            // Best-effort persist; display the change even if the DB write fails.
            let _ = store.update_model(&app.session.id, &name).await;
            app.messages.push(DisplayMessage {
                kind: MessageKind::Info,
                content: format!("Model changed to {name}"),
            });
        }
        CommandOutcome::Info(text) => {
            app.messages.push(DisplayMessage {
                kind: MessageKind::Info,
                content: text.to_string(),
            });
        }
        CommandOutcome::CommandError(text) => {
            app.messages.push(DisplayMessage {
                kind: MessageKind::Info,
                content: format!("Error: {text}"),
            });
        }
    }
    app.scroll_offset = 0;
}

/// Render the TUI.
fn draw(f: &mut Frame<'_>, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // status bar
        Constraint::Min(5),    // messages area
        Constraint::Length(3), // input area
    ])
    .split(f.area());

    draw_status_bar(f, app, chunks[0]);
    draw_messages(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);
}

/// Draw the top status bar.
fn draw_status_bar(f: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let status = Line::from(vec![
        Span::styled(
            " apx agent ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled(&app.model_name, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            format!("Session: {}", truncate_id(&app.session.id)),
            Style::default().fg(Color::DarkGray),
        ),
        if app.streaming {
            Span::styled("  streaming...", Style::default().fg(Color::Green))
        } else {
            Span::raw("")
        },
    ]);
    f.render_widget(Paragraph::new(status), area);
}

/// Draw the messages area.
fn draw_messages(f: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    for msg in &app.messages {
        let (prefix, style) = match &msg.kind {
            MessageKind::Chat(Role::User) => (
                "You: ",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            MessageKind::Chat(Role::Assistant) => ("AI:  ", Style::default().fg(Color::Green)),
            MessageKind::Info => ("  >  ", Style::default().fg(Color::Yellow)),
        };

        let prefix_span = Span::styled(prefix, style);
        let content_lines: Vec<&str> = msg.content.split('\n').collect();
        for (i, line) in content_lines.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![prefix_span.clone(), Span::raw(*line)]));
            } else {
                lines.push(Line::from(vec![Span::raw("     "), Span::raw(*line)]));
            }
        }
        lines.push(Line::raw(""));
    }

    let content_height = lines.len().saturating_sub(area.height as usize);
    let scroll = if app.scroll_offset == 0 {
        content_height as u16
    } else {
        content_height.saturating_sub(app.scroll_offset as usize) as u16
    };

    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(paragraph, area);
}

/// Draw the input area.
fn draw_input(f: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let prompt = if app.streaming {
        Span::styled(" ... ", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(" > ", Style::default().fg(Color::Yellow))
    };
    let text = Line::from(vec![prompt, Span::raw(&app.input)]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title("Send (Enter) | /help | Quit (Ctrl+C)");
    let paragraph = Paragraph::new(text).block(block);
    f.render_widget(paragraph, area);

    // Place cursor at end of input
    if !app.streaming {
        #[allow(clippy::cast_possible_truncation)]
        let cursor_x = area.x + 4 + app.input.len() as u16;
        let cursor_y = area.y + 1;
        f.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Truncate a session ID for display.
fn truncate_id(id: &str) -> &str {
    if id.len() > 8 { &id[..8] } else { id }
}
