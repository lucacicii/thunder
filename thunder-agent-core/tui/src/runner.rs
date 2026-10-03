use crate::app::App;
use crate::event::{AppEvent, EventHandler};
use crate::ui::draw;
use crate::ui::theme::Theme;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, stdout};
use std::panic;
use std::time::Duration;

pub struct TuiRunner {
    tick_rate: Duration,
}

impl TuiRunner {
    pub fn new(tick_rate_ms: u64) -> Self {
        Self {
            tick_rate: Duration::from_millis(tick_rate_ms),
        }
    }

    pub async fn run(&self, mut app: App) -> Result<(), Box<dyn std::error::Error>> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Set panic hook to restore terminal
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            default_hook(info);
        }));

        let mut events = EventHandler::new(self.tick_rate);
        let theme = Theme::default();

        app.refresh_sessions().await;

        loop {
            terminal.draw(|f| draw(f, &mut app, &theme))?;

            if let Some(event) = events.next().await {
                let sender = events.sender();
                Self::dispatch_event(&mut app, event, &sender).await;

                // Batch drain immediate streaming tokens before expensive terminal redraw
                let mut drained = 0;
                while drained < 32 {
                    if let Ok(more) = events.try_next() {
                        Self::dispatch_event(&mut app, more, &sender).await;
                        drained += 1;
                    } else {
                        break;
                    }
                }
            }

            if app.should_quit {
                app.save_current_conversation().await;
                break;
            }
        }

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        Ok(())
    }

    async fn dispatch_event(
        app: &mut App,
        event: AppEvent,
        sender: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        match event {
            AppEvent::Key(key) => {
                app.handle_key(key, sender.clone());
            }
            AppEvent::Mouse(mouse) => {
                app.handle_mouse(mouse);
            }
            AppEvent::Agent(observed) => {
                app.handle_agent_event(observed);
            }
            AppEvent::AgentFinished {
                agent_id,
                success,
                final_text,
                authoritative_messages,
                raw_messages,
                run_stats,
                finish_reason,
            } => {
                app.handle_agent_finished(
                    agent_id,
                    success,
                    final_text,
                    authoritative_messages,
                    raw_messages,
                    run_stats,
                    finish_reason,
                );
                app.save_current_conversation().await;

                // Auto-title the conversation on its first exchange (or while
                // still carrying a placeholder), mirroring the daemon.
                if success && app.should_autogenerate_title() {
                    app.spawn_title_generation(sender.clone(), false);
                }
            }
            AppEvent::UserQuestion(incoming) => {
                if let Some(existing) = app.pending_question.take() {
                    // Only one question can be pending at a time; the agent loop
                    // is single-threaded per run, so this is defensive only.
                    existing.resolve(serde_json::Value::Null);
                }
                app.pending_question = crate::ask_user::PendingQuestion::from_incoming(incoming);
            }
            AppEvent::TitleGenerated { session_id, result } => {
                match result {
                    Ok(title) if session_id == app.conversation.id => {
                        app.conversation.title = Some(title.clone());
                        app.conversation.title_source = Some("auto".to_string());
                        app.set_status_message(format!("Title: {title}"));
                        app.save_current_conversation().await;
                    }
                    Ok(_) => {
                        // A different session got titled (stale task); ignore.
                    }
                    Err(err) => {
                        app.set_status_message(format!("Title generation failed: {err}"));
                    }
                }
            }
            AppEvent::LoadSession(id) => {
                app.load_conversation(&id).await;
                app.focus = crate::app::FocusPane::Input;
                app.set_status_message(format!("Loaded session: {}", id));
            }
            AppEvent::SessionDeleted(id) => {
                app.set_status_message(format!("Deleted session: {}", id));
                app.refresh_sessions().await;
            }
            AppEvent::OpenPicker {
                kind,
                title,
                items,
                empty_message,
            } => {
                if items.is_empty() {
                    let msg = empty_message.unwrap_or_else(|| "No items found.".to_string());
                    app.conversation.add_assistant_message(Some(msg), None);
                    app.save_current_conversation().await;
                } else {
                    app.picker.open(kind, Some(title), items);
                }
            }
            AppEvent::Tick => {}
            _ => {}
        }
    }
}
