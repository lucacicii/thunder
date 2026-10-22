use crate::app::{filter_slash_commands, App, TimelineItem};
use crate::ui;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use thunder_core::{
    AgentLoopConfig, ContextPruningConfig, MiddlewarePipeline, PauseGate, ProviderClientDriver,
    ReActAgentLoop, SteerQueues, ThunderConfig,
};
use thunder_protocol::{AgentEvent, ChatMessage, EventSink, ThunderEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

enum RunnerMsg {
    AgentEvent(AgentEvent),
    RunFinished {
        messages: Vec<ChatMessage>,
        outcome: Result<String, String>,
    },
}

struct TuiEventBridge {
    tx: mpsc::UnboundedSender<RunnerMsg>,
}

impl EventSink for TuiEventBridge {
    fn on_event(&self, _event: ThunderEvent) {}

    fn on_agent_event(&self, event: AgentEvent) {
        let _ = self.tx.send(RunnerMsg::AgentEvent(event));
    }
}

pub struct TuiRunner {
    tick_rate: Duration,
}

impl TuiRunner {
    pub fn new(tick_ms: u64) -> Self {
        Self {
            tick_rate: Duration::from_millis(tick_ms.max(16)),
        }
    }

    pub async fn run(&self, mut app: App) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let (tx, mut rx) = mpsc::unbounded_channel::<RunnerMsg>();
        let mut reader = EventStream::new();
        let mut ticker = tokio::time::interval(self.tick_rate);

        loop {
            terminal.draw(|f| ui::render(f, &app))?;

            if app.should_quit {
                break;
            }

            tokio::select! {
                _ = ticker.tick() => {}
                Some(msg) = rx.recv() => {
                    match msg {
                        RunnerMsg::AgentEvent(ev) => {
                            app.apply_agent_event(ev);
                        }
                        RunnerMsg::RunFinished { messages, outcome } => {
                            app.is_running = false;
                            app.is_paused = false;
                            app.active_cancel = None;
                            app.active_pause = None;
                            app.active_steer = None;
                            app.conversation.set_messages(messages);
                            app.save_current_conversation().await;
                            if let Err(err) = outcome {
                                app.timeline.push(TimelineItem::SystemNotice {
                                    text: format!("Run ended: {err}"),
                                });
                            }
                        }
                    }
                }
                Some(Ok(Event::Key(key))) = reader.next() => {
                    // 1. If Picker Modal is open, route keys to the modal
                    if app.picker.is_open {
                        match key.code {
                            KeyCode::Esc => {
                                app.picker.close();
                            }
                            KeyCode::Up => {
                                app.picker.move_selection_up();
                            }
                            KeyCode::Down => {
                                app.picker.move_selection_down();
                            }
                            KeyCode::Backspace => {
                                app.picker.handle_backspace();
                            }
                            KeyCode::Enter => {
                                app.commit_picker_selection().await;
                            }
                            KeyCode::Char(ch) => {
                                app.picker.handle_char(ch);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // 2. If autocomplete popover is active (input starts with '/')
                    if app.input.starts_with('/') {
                        let matches = filter_slash_commands(&app.input);
                        match key.code {
                            KeyCode::Tab => {
                                if !matches.is_empty() {
                                    let cmd = matches[app.autocomplete_index % matches.len()];
                                    app.input = format!("/{} ", cmd.name);
                                    app.autocomplete_index = 0;
                                }
                                continue;
                            }
                            KeyCode::Up if !matches.is_empty() => {
                                if app.autocomplete_index == 0 {
                                    app.autocomplete_index = matches.len() - 1;
                                } else {
                                    app.autocomplete_index -= 1;
                                }
                                continue;
                            }
                            KeyCode::Down if !matches.is_empty() => {
                                app.autocomplete_index = (app.autocomplete_index + 1) % matches.len();
                                continue;
                            }
                            _ => {}
                        }
                    }

                    // 3. Normal typing and shortcuts
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                            if app.is_running {
                                app.cancel_active_run();
                            } else {
                                app.should_quit = true;
                            }
                        }
                        (KeyCode::Char('p'), m) if m.contains(KeyModifiers::CONTROL) => {
                            app.toggle_pause();
                        }
                        (KeyCode::BackTab, _) => {
                            app.toggle_mode();
                        }
                        (KeyCode::Esc, _) => {
                            if app.is_running {
                                app.cancel_active_run();
                            } else {
                                app.input.clear();
                                app.autocomplete_index = 0;
                            }
                        }
                        (KeyCode::Up, _) => {
                            app.scroll_offset = app.scroll_offset.saturating_sub(2);
                        }
                        (KeyCode::Down, _) => {
                            app.scroll_offset = app.scroll_offset.saturating_add(2);
                        }
                        (KeyCode::Backspace, _) => {
                            app.input.pop();
                            app.autocomplete_index = 0;
                        }
                        (KeyCode::Enter, _) => {
                            let line = std::mem::take(&mut app.input);
                            app.autocomplete_index = 0;
                            let trimmed = line.trim().to_string();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if app.handle_slash_command(&trimmed).await {
                                continue;
                            }
                            if app.is_running {
                                app.steer_active_run(trimmed);
                            } else {
                                spawn_agent_turn(&mut app, trimmed, tx.clone()).await;
                            }
                        }
                        (KeyCode::Char(ch), _) => {
                            app.input.push(ch);
                            app.autocomplete_index = 0;
                        }
                        _ => {}
                    }
                }
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    }
}

async fn spawn_agent_turn(app: &mut App, prompt: String, tx: mpsc::UnboundedSender<RunnerMsg>) {
    app.timeline.push(TimelineItem::User {
        text: prompt.clone(),
    });

    let Some(spec) = app.provider_registry.resolve(&app.model).cloned() else {
        app.timeline.push(TimelineItem::SystemNotice {
            text: format!("Model '{}' not found in ~/.thunder/models.json", app.model),
        });
        return;
    };

    let client = match thunder_providers::client_for(&spec, 120_000) {
        Ok(c) => c,
        Err(e) => {
            app.timeline.push(TimelineItem::SystemNotice {
                text: format!("Provider error: {e}"),
            });
            return;
        }
    };

    let cancel = CancellationToken::new();
    let pause_gate = PauseGate::new_shared();
    let steer_queues = SteerQueues::new_shared();

    app.is_running = true;
    app.is_paused = false;
    app.active_cancel = Some(cancel.clone());
    app.active_pause = Some(Arc::clone(&pause_gate));
    app.active_steer = Some(Arc::clone(&steer_queues));

    let ws_dir = app.workspace_dir.clone();
    let session_id = app.conversation.id.clone();
    let mode = app.mode.clone();
    let initial_messages = app.conversation.messages.clone();
    let registry = app.tool_registry.clone();
    let model_id = spec.id.clone();
    let warm_settings = spec.prompt_cache_warm_settings(None);
    let thinking_level = app.thinking_level.clone();

    tokio::spawn(async move {
        let cfg = ThunderConfig::load_from_workspace(&ws_dir).await;
        let mut driver_builder = ProviderClientDriver::new(Arc::clone(&client))
            .with_model(model_id.clone())
            .with_session_id(session_id.clone());
        if let Some(ref lvl) = thinking_level {
            driver_builder = driver_builder.with_thinking_level(lvl);
        }
        let driver = Arc::new(driver_builder);

        let loop_cfg = AgentLoopConfig {
            session_id: session_id.clone(),
            workspace_cwd: ws_dir,
            system_prompt: "You are Thunder Agent, a microkernel coding assistant.".to_string(),
            model: model_id,
            max_turns: cfg.agent.max_turns,
            pruning: ContextPruningConfig {
                max_context_tokens: spec.context_window.min(cfg.agent.max_context_tokens),
                ..ContextPruningConfig::default()
            },
            tool_timeout: Duration::from_millis(cfg.sandbox.timeout_ms),
            cache_warm_settings: warm_settings,
        };

        let mut mw = MiddlewarePipeline::new();
        if mode == "plan" {
            let _ = registry
                .register_provider(thunder_plan::PlanToolProvider::shared(None))
                .await;
            mw = mw.with_middleware(Arc::new(thunder_plan::PlanModeMiddleware));
        }

        let bridge = Arc::new(TuiEventBridge { tx: tx.clone() });
        let mut agent = ReActAgentLoop::new(loop_cfg, registry, driver)
            .with_summarizer_client(client)
            .with_initial_messages(initial_messages)
            .with_cancellation_token(cancel)
            .with_pause_gate(pause_gate)
            .with_steer_queues(steer_queues)
            .with_middleware(mw)
            .with_event_sink(bridge);

        let outcome = agent.run(prompt).await.map(|o| o.final_response);
        let messages = agent.context().get_messages();
        let _ = tx.send(RunnerMsg::RunFinished { messages, outcome });
    });
}
