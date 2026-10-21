use std::path::PathBuf;
use std::sync::Arc;
use thunder_core::{
    now_ms, Conversation, ConversationFilter, ConversationStore, FsConversationStore, PauseGate,
    SteerQueues,
};
use thunder_protocol::{AgentEvent, ChatMessage, Role};
use thunder_providers::prelude::ProviderRegistry;
use thunder_registry::ToolRegistry;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCardStatus {
    Running,
    Success {
        output_preview: String,
        duration_ms: u64,
    },
    Error {
        error_preview: String,
        duration_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineItem {
    User {
        text: String,
    },
    Assistant {
        content: String,
        reasoning: String,
        streaming: bool,
    },
    ToolCallCard {
        call_id: String,
        name: String,
        args_summary: String,
        status: ToolCardStatus,
    },
    SystemNotice {
        text: String,
    },
}

pub struct App {
    pub model: String,
    pub mode: String,
    pub workspace_dir: PathBuf,
    pub conversation: Conversation,
    pub store: Option<Arc<FsConversationStore>>,
    pub provider_registry: ProviderRegistry,
    pub tool_registry: ToolRegistry,
    pub timeline: Vec<TimelineItem>,
    pub input: String,
    pub scroll_offset: u16,
    pub status_message: String,
    pub estimated_tokens: usize,
    pub is_running: bool,
    pub is_paused: bool,
    pub active_cancel: Option<CancellationToken>,
    pub active_pause: Option<Arc<PauseGate>>,
    pub active_steer: Option<Arc<SteerQueues>>,
    pub should_quit: bool,
}

impl App {
    pub fn new(model: impl Into<String>) -> Self {
        let model_str = model.into();
        let workspace_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let session_id = format!("sess-{}", now_ms());
        let conversation = Conversation::new(&session_id)
            .with_model(model_str.clone())
            .with_workspace(workspace_dir.to_string_lossy().into_owned());

        Self {
            model: model_str,
            mode: "yolo".to_string(),
            workspace_dir,
            conversation,
            store: None,
            provider_registry: ProviderRegistry::default(),
            tool_registry: ToolRegistry::new(),
            timeline: vec![TimelineItem::SystemNotice {
                text: "Welcome to Thunder TUI (Microkernel Edition). Type /help for commands."
                    .to_string(),
            }],
            input: String::new(),
            scroll_offset: 0,
            status_message: "Ready".to_string(),
            estimated_tokens: 0,
            is_running: false,
            is_paused: false,
            active_cancel: None,
            active_pause: None,
            active_steer: None,
            should_quit: false,
        }
    }

    pub fn with_store(mut self, store: FsConversationStore) -> Self {
        self.store = Some(Arc::new(store));
        self
    }

    pub fn with_provider_registry(mut self, registry: ProviderRegistry) -> Self {
        self.provider_registry = registry;
        self
    }

    pub fn with_tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = registry;
        self
    }

    pub fn set_status_message(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
    }

    pub fn toggle_mode(&mut self) {
        self.mode = if self.mode == "yolo" {
            "plan".to_string()
        } else {
            "yolo".to_string()
        };
        self.set_status_message(format!("Switched mode to '{}'", self.mode));
    }

    pub fn toggle_pause(&mut self) {
        if !self.is_running {
            return;
        }
        if let Some(ref gate) = self.active_pause {
            if self.is_paused {
                gate.resume();
                self.is_paused = false;
                self.set_status_message("Resumed execution");
            } else {
                gate.pause();
                self.is_paused = true;
                self.set_status_message("Paused at tool boundary (Ctrl+P to resume)");
            }
        }
    }

    pub fn cancel_active_run(&mut self) {
        if let Some(ref cancel) = self.active_cancel {
            cancel.cancel();
        }
        if let Some(ref gate) = self.active_pause {
            gate.resume();
        }
        self.is_paused = false;
        self.set_status_message("Cancelling active run...");
    }

    pub async fn save_current_conversation(&self) {
        if let Some(ref store) = self.store {
            let _ = store.save(&self.conversation).await;
        }
    }

    pub async fn load_conversation(&mut self, session_id: &str) {
        let Some(ref store) = self.store else {
            return;
        };
        if let Ok(Some(conv)) = store.load(session_id).await {
            if let Some(ref m) = conv.model {
                self.model = m.clone();
            }
            self.timeline.clear();
            for msg in &conv.messages {
                match msg.role() {
                    Role::User => {
                        if let Some(text) = msg.content_str() {
                            self.timeline.push(TimelineItem::User {
                                text: text.to_string(),
                            });
                        }
                    }
                    Role::Assistant => {
                        let content = msg.content_str().unwrap_or_default().to_string();
                        let reasoning = msg.reasoning_str().unwrap_or_default().to_string();
                        if !content.is_empty() || !reasoning.is_empty() {
                            self.timeline.push(TimelineItem::Assistant {
                                content,
                                reasoning,
                                streaming: false,
                            });
                        }
                    }
                    Role::System | Role::Tool => {}
                }
            }
            self.estimated_tokens = conv.stats.total_tokens;
            self.conversation = conv;
            self.set_status_message(format!("Loaded session {session_id}"));
        } else {
            self.set_status_message(format!("Session '{session_id}' not found"));
        }
    }

    /// Apply a streaming `AgentEvent` from `ReActAgentLoop` onto the TUI timeline.
    pub fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart {
                turn,
                estimated_prompt_tokens,
                ..
            } => {
                self.estimated_tokens = estimated_prompt_tokens;
                self.set_status_message(format!("Turn {turn} · ~{estimated_prompt_tokens} tokens"));
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                if let Some(TimelineItem::Assistant {
                    reasoning,
                    streaming: true,
                    ..
                }) = self.timeline.last_mut()
                {
                    reasoning.push_str(&delta);
                } else {
                    self.timeline.push(TimelineItem::Assistant {
                        content: String::new(),
                        reasoning: delta,
                        streaming: true,
                    });
                }
            }
            AgentEvent::TokenDelta { delta, .. } => {
                if let Some(TimelineItem::Assistant {
                    content,
                    streaming: true,
                    ..
                }) = self.timeline.last_mut()
                {
                    content.push_str(&delta);
                } else {
                    self.timeline.push(TimelineItem::Assistant {
                        content: delta,
                        reasoning: String::new(),
                        streaming: true,
                    });
                }
            }
            AgentEvent::ToolExecStart {
                tool_call_id,
                name,
                arguments,
                ..
            } => {
                let args_summary = summarize_line(&arguments.to_string(), 80);
                self.timeline.push(TimelineItem::ToolCallCard {
                    call_id: tool_call_id,
                    name: name.clone(),
                    args_summary,
                    status: ToolCardStatus::Running,
                });
                self.set_status_message(format!("Running tool {name}..."));
            }
            AgentEvent::ToolExecResult {
                tool_call_id,
                result,
                ..
            } => {
                let preview = summarize_line(&result.output, 120);
                for item in self.timeline.iter_mut().rev() {
                    if let TimelineItem::ToolCallCard {
                        call_id, status, ..
                    } = item
                    {
                        if *call_id == tool_call_id {
                            *status = if result.is_error {
                                ToolCardStatus::Error {
                                    error_preview: preview,
                                    duration_ms: result.duration_ms,
                                }
                            } else {
                                ToolCardStatus::Success {
                                    output_preview: preview,
                                    duration_ms: result.duration_ms,
                                }
                            };
                            break;
                        }
                    }
                }
            }
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                ..
            } => {
                self.estimated_tokens = tokens_after;
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!(
                        "Context compacted via checkpoint: {tokens_before} -> {tokens_after} tokens"
                    ),
                });
            }
            AgentEvent::SteerAccepted {
                behavior, message, ..
            } => {
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!("[{behavior}] Injected: {message}"),
                });
            }
            AgentEvent::LoopComplete { stats, .. } => {
                for item in self.timeline.iter_mut().rev() {
                    if let TimelineItem::Assistant { streaming, .. } = item {
                        *streaming = false;
                        break;
                    }
                }
                self.set_status_message(format!(
                    "Completed in {} turn(s) ({} ms)",
                    stats.total_turns, stats.total_duration_ms
                ));
            }
            AgentEvent::Error { message, .. } => {
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!("Error: {message}"),
                });
            }
            _ => {}
        }
    }

    /// Execute a slash command (starts with `/`). Returns `true` if handled as a slash command.
    pub async fn handle_slash_command(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if !trimmed.starts_with('/') {
            return false;
        }

        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().map(str::trim).unwrap_or("");

        match cmd {
            "/help" => {
                self.timeline.push(TimelineItem::SystemNotice {
                    text: "Commands: /model [id] · /tools · /mode [yolo|plan] · /new · /sessions · /load <id> · /clear · /quit".to_string(),
                });
            }
            "/clear" => {
                self.timeline.clear();
                self.set_status_message("Cleared timeline");
            }
            "/new" => {
                let sid = format!("sess-{}", now_ms());
                self.conversation = Conversation::new(&sid)
                    .with_model(self.model.clone())
                    .with_workspace(self.workspace_dir.to_string_lossy().into_owned());
                self.timeline.clear();
                self.estimated_tokens = 0;
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!("Started new session {sid}"),
                });
            }
            "/mode" => {
                if arg.is_empty() {
                    self.toggle_mode();
                } else if matches!(arg, "yolo" | "plan" | "ask" | "acceptEdits") {
                    self.mode = arg.to_string();
                    self.set_status_message(format!("Mode set to '{}'", self.mode));
                } else {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!("Unknown mode '{arg}'. Valid modes: yolo, plan"),
                    });
                }
            }
            "/model" => {
                if arg.is_empty() {
                    let list: Vec<String> = self
                        .provider_registry
                        .list_available()
                        .iter()
                        .map(|m| {
                            format!(
                                "{} ({})",
                                m.selection_id(),
                                if m.available { "ready" } else { "no-auth" }
                            )
                        })
                        .collect();
                    let text = if list.is_empty() {
                        format!("Current model: {} (no catalog models loaded)", self.model)
                    } else {
                        format!("Current: {}\nAvailable: {}", self.model, list.join(", "))
                    };
                    self.timeline.push(TimelineItem::SystemNotice { text });
                } else {
                    self.model = arg.to_string();
                    self.conversation.model = Some(self.model.clone());
                    self.set_status_message(format!("Switched model to '{}'", self.model));
                }
            }
            "/tools" => {
                let snap = self.tool_registry.current();
                let mut names: Vec<String> = snap
                    .descriptors
                    .values()
                    .map(|d| format!("{} ({})", d.name, d.uri))
                    .collect();
                names.sort();
                let text = format!(
                    "CatalogSnapshot v{} ({} tools): {}",
                    snap.version,
                    names.len(),
                    if names.is_empty() {
                        "none".to_string()
                    } else {
                        names.join(", ")
                    }
                );
                self.timeline.push(TimelineItem::SystemNotice { text });
            }
            "/sessions" => {
                if let Some(ref store) = self.store {
                    let filter = ConversationFilter::new().with_min_turns(1);
                    if let Ok(list) = store.list(&filter).await {
                        let summary = if list.is_empty() {
                            "No saved sessions.".to_string()
                        } else {
                            list.iter()
                                .take(10)
                                .map(|s| {
                                    format!(
                                        "{} ({})",
                                        s.id,
                                        s.title.as_deref().unwrap_or("Untitled")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" | ")
                        };
                        self.timeline
                            .push(TimelineItem::SystemNotice { text: summary });
                    }
                }
            }
            "/load" => {
                if !arg.is_empty() {
                    self.load_conversation(arg).await;
                }
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            other => {
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!("Unknown command '{other}'. Type /help for available commands."),
                });
            }
        }
        true
    }

    /// Enqueue a steering message into an active run when the user presses Enter while running.
    pub fn steer_active_run(&mut self, text: String) {
        if let Some(ref queues) = self.active_steer {
            queues.steering.enqueue(ChatMessage::user(text.clone()));
            self.timeline.push(TimelineItem::SystemNotice {
                text: format!("Queued steering message: {text}"),
            });
            self.set_status_message("Steering message queued for next turn boundary");
        }
    }
}

fn summarize_line(raw: &str, max_chars: usize) -> String {
    let single = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= max_chars {
        single
    } else {
        let head: String = single.chars().take(max_chars).collect();
        format!("{head}…")
    }
}
