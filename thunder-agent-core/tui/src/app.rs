use crate::picker::{PickerItem, PickerKind, PickerResult, PickerState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_providers::prelude::*;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Host/test seam for resolving the LLM client of a run.
///
/// Production leaves this unset: clients are resolved from the provider
/// registry. Tests inject a fake client here instead of relying on a shipped
/// "mock mode", so no mock code has to live in the runtime path.
pub type ClientFactory = Arc<dyn Fn(&AgentConfig) -> Option<Arc<dyn LLMClientTrait>> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Chat,
    SessionList,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Input,
    Chat,
    Sidebar,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    #[default]
    AutoRouter,
    SingleAgent,
}

impl ExecutionMode {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::AutoRouter => "⚡ Auto",
            Self::SingleAgent => "Single",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::AutoRouter => "Plugin-Host Agent (Conversation + Skills + MCP)",
            Self::SingleAgent => "Single Agent Direct Run",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::AutoRouter => Self::SingleAgent,
            Self::SingleAgent => Self::AutoRouter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool { name: String, duration_ms: u64 },
    Done,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ActiveToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub is_error: bool,
    pub duration_ms: u64,
}

pub struct App {
    pub mode: ViewMode,
    pub focus: FocusPane,
    pub execution_mode: ExecutionMode,
    pub conversation: Conversation,
    pub session_list: Vec<ConversationSummary>,
    pub selected_session_idx: usize,
    pub input: String,
    pub input_history: Vec<String>,
    pub history_idx: Option<usize>,
    pub command_popup_idx: usize,
    pub picker: PickerState,
    pub workspace_dir: PathBuf,
    pub temperature: f32,
    pub max_turns: usize,
    pub request_timeout_ms: u64,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub agent_status: AgentStatus,
    pub streaming_delta: String,
    pub reasoning_delta: String,
    pub active_tool_calls: Vec<ActiveToolCall>,
    pub model: ModelRef,
    pub provider_registry: ProviderRegistry,
    pub show_sidebar: bool,
    pub should_quit: bool,
    /// Optional client factory (tests / embedders). `None` = provider registry.
    pub client_factory: Option<ClientFactory>,
    pub store: Option<FsConversationStore>,
    pub cancel_token: Option<CancellationToken>,
    pub status_message: Option<(String, std::time::Instant)>,
    pub last_error: Option<String>,
    pub last_max_scroll: usize,
    pub active_skill: Option<thunder_agent_skills::SkillHandle>,
    /// Raw pre-compaction transcript pending a sidecar write (checkpoint mode).
    pub pending_raw_transcript: Option<Vec<ChatMessage>>,
}

impl App {
    pub fn new(model_name: impl Into<String>) -> Self {
        let model = ModelRef::parse(&model_name.into());
        let initial_conv = Conversation::new(format!("sess_{}", now_ms()))
            .with_title("New Conversation")
            .with_system_prompt("You are a helpful, fast, autonomous AI engineering assistant.");

        Self {
            mode: ViewMode::Chat,
            focus: FocusPane::Input,
            execution_mode: ExecutionMode::AutoRouter,
            conversation: initial_conv,
            session_list: Vec::new(),
            selected_session_idx: 0,
            input: String::new(),
            input_history: Vec::new(),
            history_idx: None,
            command_popup_idx: 0,
            picker: PickerState::new(),
            workspace_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            temperature: 0.2,
            max_turns: 30,
            request_timeout_ms: 120_000,
            scroll_offset: 0,
            auto_scroll: true,
            agent_status: AgentStatus::Idle,
            streaming_delta: String::new(),
            reasoning_delta: String::new(),
            active_tool_calls: Vec::new(),
            model,
            provider_registry: ProviderRegistry::default(),
            show_sidebar: false,
            should_quit: false,
            client_factory: None,
            store: None,
            cancel_token: None,
            status_message: None,
            last_error: None,
            last_max_scroll: 0,
            active_skill: None,
            pending_raw_transcript: None,
        }
    }

    pub fn with_store(mut self, store: FsConversationStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Inject a client factory. Tests use this to drive runs against a fake
    /// `LLMClientTrait` without any mock code shipping in the runtime path.
    pub fn with_client_factory(mut self, factory: ClientFactory) -> Self {
        self.client_factory = Some(factory);
        self
    }

    pub fn with_provider_registry(mut self, registry: ProviderRegistry) -> Self {
        self.provider_registry = registry;
        self
    }

    pub async fn refresh_sessions(&mut self) {
        if let Some(store) = &self.store {
            if let Ok(list) = store.list(&ConversationFilter::new()).await {
                self.session_list = list;
                if self.selected_session_idx >= self.session_list.len()
                    && !self.session_list.is_empty()
                {
                    self.selected_session_idx = self.session_list.len() - 1;
                }
            }
        }
    }

    pub async fn save_current_conversation(&mut self) {
        if let Some(store) = &self.store {
            let _ = store.save(&self.conversation).await;
            // Flush any raw pre-compaction transcript captured this run
            // (sidecar file; the projection stays the working history).
            if let Some(raw) = self.pending_raw_transcript.take() {
                let session_id = self.conversation.id.clone();
                if let Err(e) = store.save_raw_transcript(&session_id, &raw).await {
                    tracing::warn!(error = %e, session_id = %session_id, "failed to persist raw transcript");
                }
            }
            self.refresh_sessions().await;
        }
    }

    pub fn save_and_refresh(&mut self) {
        if let Some(store) = self.store.clone() {
            let conv = self.conversation.clone();
            tokio::spawn(async move {
                let _ = store.save(&conv).await;
            });
        }
    }

    pub async fn load_conversation(&mut self, id: &str) {
        if let Some(store) = &self.store {
            // Save current in-memory conversation first
            let _ = store.save(&self.conversation).await;

            if let Ok(Some(loaded)) = store.load(id).await {
                self.conversation = loaded;
                self.streaming_delta.clear();
                self.reasoning_delta.clear();
                self.active_tool_calls.clear();
                self.agent_status = AgentStatus::Idle;
                self.scroll_offset = 0;
                self.last_error = None;
            }
            self.refresh_sessions().await;
            if let Some(pos) = self.session_list.iter().position(|s| s.id == id) {
                self.selected_session_idx = pos;
            }
        }
    }

    pub fn set_status_message(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), std::time::Instant::now()));
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        // 1. If interactive picker dropdown is active, route all keys to the picker
        if self.picker.is_open {
            match self.picker.handle_key(key) {
                PickerResult::Selected(kind, item) => {
                    self.handle_picker_selection(kind, item, event_tx);
                }
                PickerResult::Cancelled => {
                    self.set_status_message("Selection closed.");
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(token) = self.cancel_token.take() {
                    token.cancel();
                    self.set_status_message("Agent execution cancelled.");
                    self.agent_status = AgentStatus::Idle;
                    self.streaming_delta.clear();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.new_session();
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.execution_mode = self.execution_mode.next();
                self.set_status_message(format!("Mode: {}", self.execution_mode.description()));
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_sidebar = !self.show_sidebar;
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = match self.mode {
                    ViewMode::Help => ViewMode::Chat,
                    _ => ViewMode::Help,
                };
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
            }
            KeyCode::Home
                if key.modifiers.contains(KeyModifiers::CONTROL) || self.input.is_empty() =>
            {
                self.scroll_to_top();
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL) || self.input.is_empty() =>
            {
                self.scroll_to_bottom();
            }
            KeyCode::Up
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.scroll_up(1);
            }
            KeyCode::Down
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.scroll_down(1);
            }
            KeyCode::Tab => {
                if self.focus == FocusPane::Input && self.input.starts_with('/') {
                    let matches = crate::commands::filter_commands(&self.input);
                    if !matches.is_empty() {
                        let selected = matches[self.command_popup_idx % matches.len()];
                        self.input = format!("/{} ", selected.name);
                        return;
                    }
                }
                self.cycle_focus();
            }
            KeyCode::Esc => match self.mode {
                ViewMode::Help | ViewMode::SessionList => {
                    self.mode = ViewMode::Chat;
                    self.focus = FocusPane::Input;
                }
                ViewMode::Chat => {
                    if self.input.starts_with('/') {
                        self.input.clear();
                        self.command_popup_idx = 0;
                    } else if let Some(token) = self.cancel_token.take() {
                        token.cancel();
                        self.set_status_message("Cancelled.");
                        self.agent_status = AgentStatus::Idle;
                        self.streaming_delta.clear();
                    }
                }
            },
            _ => match self.focus {
                FocusPane::Input => self.handle_input_key(key, event_tx),
                FocusPane::Chat => self.handle_chat_scroll(key),
                FocusPane::Sidebar => self.handle_sidebar_key(key, event_tx),
                FocusPane::Monitor => self.handle_monitor_key(key),
            },
        }
    }

    fn handle_input_key(
        &mut self,
        key: KeyEvent,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        match key.code {
            KeyCode::Enter => {
                if self.input.starts_with('/') {
                    let trimmed = self.input.trim();
                    let matches = crate::commands::filter_commands(&self.input);

                    let token = trimmed
                        .trim_start_matches('/')
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    let is_exact_command = crate::commands::ALL_COMMANDS
                        .iter()
                        .any(|cmd| cmd.matches(token));

                    // If input is just "/" or an incomplete partial prefix, Enter autocompletes the selected command
                    if (trimmed == "/" || (!is_exact_command && !matches.is_empty()))
                        && !matches.is_empty()
                    {
                        let selected = matches[self.command_popup_idx % matches.len()];
                        self.input = format!("/{} ", selected.name);
                        self.command_popup_idx = 0;
                        return;
                    }
                }

                if !self.input.trim().is_empty()
                    && (self.agent_status == AgentStatus::Idle
                        || matches!(self.agent_status, AgentStatus::Done | AgentStatus::Error(_)))
                {
                    let prompt = std::mem::take(&mut self.input);
                    self.command_popup_idx = 0;
                    self.input_history.push(prompt.clone());
                    self.history_idx = None;

                    // Execute slash command locally if recognized
                    if self.execute_slash_command(&prompt, event_tx.clone()) {
                        return;
                    }

                    self.submit_prompt(prompt, event_tx);
                }
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.command_popup_idx = 0;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.command_popup_idx = 0;
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
            }
            KeyCode::Home if self.input.is_empty() => {
                self.scroll_to_top();
            }
            KeyCode::End if self.input.is_empty() => {
                self.scroll_to_bottom();
            }
            KeyCode::Up => {
                if self.input.starts_with('/') {
                    let matches = crate::commands::filter_commands(&self.input);
                    if !matches.is_empty() {
                        self.command_popup_idx = if self.command_popup_idx == 0 {
                            matches.len().saturating_sub(1)
                        } else {
                            self.command_popup_idx - 1
                        };
                        return;
                    }
                }

                if !self.input_history.is_empty() {
                    let next_idx = match self.history_idx {
                        Some(idx) if idx > 0 => idx - 1,
                        Some(_) => 0,
                        None => self.input_history.len() - 1,
                    };
                    self.history_idx = Some(next_idx);
                    self.input = self.input_history[next_idx].clone();
                } else if self.input.is_empty() {
                    self.scroll_up(1);
                }
            }
            KeyCode::Down => {
                if self.input.starts_with('/') {
                    let matches = crate::commands::filter_commands(&self.input);
                    if !matches.is_empty() {
                        self.command_popup_idx = (self.command_popup_idx + 1) % matches.len();
                        return;
                    }
                }

                if let Some(idx) = self.history_idx {
                    if idx + 1 < self.input_history.len() {
                        let next_idx = idx + 1;
                        self.history_idx = Some(next_idx);
                        self.input = self.input_history[next_idx].clone();
                    } else {
                        self.history_idx = None;
                        self.input.clear();
                    }
                } else if self.input.is_empty() {
                    self.scroll_down(1);
                }
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        match mouse.kind {
            crossterm::event::MouseEventKind::ScrollUp => {
                if self.picker.is_open {
                    self.picker.move_up();
                } else {
                    self.scroll_up(3);
                }
            }
            crossterm::event::MouseEventKind::ScrollDown => {
                if self.picker.is_open {
                    self.picker.move_down();
                } else {
                    self.scroll_down(3);
                }
            }
            _ => {}
        }
    }

    pub fn scroll_up(&mut self, lines: usize) {
        let current = if self.auto_scroll {
            self.last_max_scroll
        } else {
            self.scroll_offset
        };
        self.scroll_offset = current.saturating_sub(lines);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, lines: usize) {
        if !self.auto_scroll {
            self.scroll_offset += lines;
            if self.scroll_offset >= self.last_max_scroll {
                self.auto_scroll = true;
                self.scroll_offset = self.last_max_scroll;
            }
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.auto_scroll = true;
        self.scroll_offset = self.last_max_scroll;
    }

    fn handle_chat_scroll(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_up(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_down(1);
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
            }
            KeyCode::End => {
                self.scroll_to_bottom();
            }
            KeyCode::Home => {
                self.scroll_to_top();
            }
            _ => {}
        }
    }

    fn handle_sidebar_key(
        &mut self,
        key: KeyEvent,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_session_idx > 0 {
                    self.selected_session_idx -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.session_list.is_empty()
                    && self.selected_session_idx + 1 < self.session_list.len()
                {
                    self.selected_session_idx += 1;
                }
            }
            KeyCode::Enter => {
                if let Some(summary) = self.session_list.get(self.selected_session_idx) {
                    let _ = event_tx.send(crate::event::AppEvent::LoadSession(summary.id.clone()));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(summary) = self.session_list.get(self.selected_session_idx) {
                    let id = summary.id.clone();
                    if let Some(store) = self.store.clone() {
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let _ = store.delete(&id).await;
                            let _ = tx.send(crate::event::AppEvent::SessionDeleted(id));
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_monitor_key(&mut self, _key: KeyEvent) {}

    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Input => FocusPane::Chat,
            FocusPane::Chat => {
                if self.show_sidebar {
                    FocusPane::Sidebar
                } else {
                    FocusPane::Input
                }
            }
            FocusPane::Sidebar => FocusPane::Input,
            FocusPane::Monitor => FocusPane::Input,
        };
    }

    fn spawn_skills_picker(&mut self, event_tx: mpsc::UnboundedSender<crate::event::AppEvent>) {
        self.set_status_message("Scanning skills directories...");
        tokio::spawn(async move {
            let (skills, _) =
                thunder_agent_skills::loader::SkillLoader::scan_default_and_report().await;
            let items: Vec<PickerItem> = skills
                .into_iter()
                .map(|s| {
                    let tag = s
                        .tags
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "skill".to_string());
                    let brief = s.description.lines().next().unwrap_or("").to_string();
                    PickerItem::new(&s.name, &s.name, brief).with_badge(tag)
                })
                .collect();
            let _ = event_tx.send(crate::event::AppEvent::OpenPicker {
                kind: PickerKind::SelectSkill,
                title: "📖 Attach skill handler (↑/↓ to move, Enter to attach)".to_string(),
                items,
                empty_message: Some(
                    "No skills found in `~/.agents/skills`, `~/.pi/agent/.../skills`, `.agents/skills` or `skills/`.".to_string(),
                ),
            });
        });
    }

    fn spawn_model_picker(&mut self, event_tx: mpsc::UnboundedSender<crate::event::AppEvent>) {
        self.set_status_message("Loading model catalog...");
        tokio::spawn(async move {
            let items = match ProviderRegistry::load_default().await {
                Ok(registry) => registry
                    .list_available()
                    .into_iter()
                    .map(|spec| {
                        PickerItem::new(
                            spec.selection_id(),
                            spec.picker_title(),
                            format!("{} · {}", spec.api.label(), spec.base_url),
                        )
                        .with_badge(&spec.provider)
                    })
                    .collect::<Vec<_>>(),
                Err(_) => fallback_model_items(),
            };
            let items = if items.is_empty() {
                fallback_model_items()
            } else {
                items
            };
            let _ = event_tx.send(crate::event::AppEvent::OpenPicker {
                kind: PickerKind::SelectModel,
                title: "🧠 Select Active LLM Model (↑/↓ to move, Enter to select)".to_string(),
                items,
                empty_message: Some(
                    "No models available. Add ~/.thunder/models.json or configure provider auth."
                        .to_string(),
                ),
            });
        });
    }

    fn spawn_mcp_picker(&mut self, event_tx: mpsc::UnboundedSender<crate::event::AppEvent>) {
        self.set_status_message("Scanning MCP configuration files...");
        let ws = self.workspace_dir.clone();
        tokio::spawn(async move {
            let cfg_opt =
                thunder_agent_mcp::config::McpConfig::find_and_load_from_workspace(&ws).await;
            let items = if let Some((path, cfg)) = cfg_opt {
                cfg.mcp_servers
                    .into_iter()
                    .map(|(name, srv)| {
                        PickerItem::new(
                            &name,
                            &name,
                            format!(
                                "{} {}  ({})",
                                srv.command,
                                srv.args.join(" "),
                                path.display()
                            ),
                        )
                        .with_badge(if srv.disabled {
                            "Disabled"
                        } else {
                            "Active"
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let _ = event_tx.send(crate::event::AppEvent::OpenPicker {
                kind: PickerKind::SelectMcp,
                title: "🔌 Connected MCP Servers (↑/↓ to move, Enter to inspect)".to_string(),
                items,
                empty_message: Some(
                    "No MCP servers found. Looked in workspace `.mcp.json` / `mcp_servers.json`, plus `~/.cursor/mcp.json` and Claude Desktop config.".to_string(),
                ),
            });
        });
    }

    fn attach_skill_handler(&mut self, name: &str, description: &str) {
        let handle = thunder_agent_skills::SkillHandle::from_picker(name, description);
        let confirm = handle.confirm_line();
        self.active_skill = Some(handle);
        self.conversation
            .add_user_message(format!("/skills {name}"));
        self.conversation.add_assistant_message(Some(confirm), None);
        self.set_status_message(format!("Skill attached: {name}"));
        self.save_and_refresh();
    }

    fn detach_skill_handler(&mut self) {
        let previous = self.active_skill.take();
        let msg = if let Some(prev) = previous {
            format!("Detached skill handler **{}**.", prev.name)
        } else {
            "No skill handler is currently attached.".to_string()
        };
        self.conversation.add_assistant_message(Some(msg), None);
        self.set_status_message("Skill detached");
        self.save_and_refresh();
    }

    pub fn new_session(&mut self) {
        let new_id = format!("sess_{}", now_ms());
        self.conversation = Conversation::new(new_id)
            .with_title("New Conversation")
            .with_system_prompt("You are a helpful, fast, autonomous AI engineering assistant.");
        self.streaming_delta.clear();
        self.reasoning_delta.clear();
        self.active_tool_calls.clear();
        self.agent_status = AgentStatus::Idle;
        self.scroll_offset = 0;
        self.last_error = None;
        self.active_skill = None;
        self.set_status_message("Created new session.");

        self.save_and_refresh();
    }

    pub fn handle_picker_selection(
        &mut self,
        kind: PickerKind,
        item: PickerItem,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        match kind {
            PickerKind::ResumeSession => {
                let id = item.id.clone();
                let _ = event_tx.send(crate::event::AppEvent::LoadSession(id));
            }
            PickerKind::SelectModel => {
                let new_model = item.id.clone();
                let old = self.model.selection_id();
                self.model = ModelRef::parse(&new_model);
                self.conversation
                    .add_user_message(format!("/model {new_model}"));
                self.conversation.add_assistant_message(
                    Some(format!(
                        "✔ Active LLM model switched from `{old}` to **`{new_model}`**."
                    )),
                    None,
                );
                self.set_status_message(format!("Switched model to {}", new_model));
                self.save_and_refresh();
            }
            PickerKind::SelectMode => {
                let new_mode = match item.id.as_str() {
                    "single" => ExecutionMode::SingleAgent,
                    _ => ExecutionMode::AutoRouter,
                };
                self.execution_mode = new_mode;
                self.conversation
                    .add_user_message(format!("/mode {}", item.id));
                self.conversation.add_assistant_message(
                    Some(format!(
                        "✔ Execution mode switched to **`{}`** ({})",
                        self.execution_mode.badge(),
                        self.execution_mode.description()
                    )),
                    None,
                );
                self.set_status_message(format!("Mode: {}", self.execution_mode.description()));
                self.save_and_refresh();
            }
            PickerKind::SelectSkill => {
                self.attach_skill_handler(&item.id, &item.description);
                let _ = event_tx;
            }
            PickerKind::SelectMcp => {
                self.conversation
                    .add_user_message(format!("/mcp show {}", item.id));
                self.conversation.add_assistant_message(
                    Some(format!(
                        "### 🔌 MCP Server Endpoint: `{}`\n\n- **Status**: {}\n- **Command**: {}",
                        item.title,
                        item.badge.as_deref().unwrap_or("Active"),
                        item.description
                    )),
                    None,
                );
                self.save_and_refresh();
            }
            PickerKind::SlashCommand => {
                self.input = format!("/{} ", item.id);
            }
        }
    }

    pub fn execute_slash_command(
        &mut self,
        raw_cmd: &str,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) -> bool {
        let trimmed = raw_cmd.trim();
        if !trimmed.starts_with('/') {
            return false;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.is_empty() {
            return false;
        }

        let cmd_token = parts[0].trim_start_matches('/');
        let args = &parts[1..];

        match cmd_token.to_lowercase().as_str() {
            // 1. /resume [session_id | #]
            "resume" | "load_session" | "switch" | "sessions" => {
                if let Some(target) = args.first() {
                    self.conversation.add_user_message(raw_cmd);
                    let target_str = target.to_string();
                    let target_clone = target_str.clone();
                    let store = self.store.clone();
                    let (tx, mut rx) = mpsc::unbounded_channel::<Option<Conversation>>();

                    tokio::spawn(async move {
                        if let Some(s) = store {
                            let mgr = ConversationManager::new(s);
                            let loaded = mgr.resume(&target_clone).await.ok().flatten();
                            let _ = tx.send(loaded);
                        } else {
                            let _ = tx.send(None);
                        }
                    });

                    let event_tx_clone = event_tx.clone();
                    let target_name = target_str.clone();
                    tokio::spawn(async move {
                        if let Some(conv_opt) = rx.recv().await {
                            if let Some(conv) = conv_opt {
                                let _ = event_tx_clone
                                    .send(crate::event::AppEvent::LoadSession(conv.id));
                            } else {
                                let _ = event_tx_clone.send(crate::event::AppEvent::AgentFinished {
                                    agent_id: "session_manager".to_string(),
                                    success: false,
                                    final_text: Some(format!("❌ Could not find session `{target_name}` to resume. Use `/resume` to view all saved sessions.")),
                                    authoritative_messages: None,
                                raw_messages: None,
                                });
                            }
                        }
                    });
                } else {
                    // Open interactive dropdown picker for sessions
                    let items: Vec<PickerItem> = self
                        .session_list
                        .iter()
                        .map(|s| {
                            let title = s.title.as_deref().unwrap_or("Untitled Conversation");
                            let brief_title =
                                title.lines().next().unwrap_or("Untitled").to_string();
                            PickerItem::new(
                                &s.id,
                                brief_title,
                                format!("ID: {} | Updated: {}", s.id, s.updated_at_ms),
                            )
                            .with_badge(format!("{} turns", s.turn_count))
                        })
                        .collect();

                    if items.is_empty() {
                        self.conversation.add_user_message(raw_cmd);
                        self.conversation.add_assistant_message(
                            Some("No previous conversation sessions found in storage.".to_string()),
                            None,
                        );
                        self.save_and_refresh();
                    } else {
                        self.picker.open(
                            PickerKind::ResumeSession,
                            Some(
                                "📁 Select Session to Resume (↑/↓ to move, Enter to resume)"
                                    .to_string(),
                            ),
                            items,
                        );
                    }
                }
                true
            }

            // 2. /help or /?
            "help" | "?" => {
                let mut out = String::from("### ⚡ Thunder TUI Slash Commands Reference\n\n");
                out.push_str("| Command | Arguments | Description |\n");
                out.push_str("|---|---|---|\n");
                for cmd in crate::commands::ALL_COMMANDS {
                    let aliases_str = if !cmd.aliases.is_empty() {
                        format!(" (`/{}`)", cmd.aliases.join(", /"))
                    } else {
                        String::new()
                    };
                    out.push_str(&format!(
                        "| `/{}`{} | `{}` | {} |\n",
                        cmd.name, aliases_str, cmd.args_hint, cmd.description
                    ));
                }
                out.push_str("\n**Keyboard Shortcuts:**\n");
                out.push_str("- `Ctrl+N`: New Session | `Ctrl+P`: Cycle Mode | `Tab`: Autocomplete / Cycle Focus\n");
                out.push_str("- `Ctrl+B`: Toggle Sidebar | `Ctrl+M`: Toggle Monitor | `Ctrl+C / Esc`: Cancel\n");

                self.conversation.add_user_message(raw_cmd);
                self.conversation.add_assistant_message(Some(out), None);
                self.set_status_message("Displayed help reference.");
                self.save_and_refresh();
                true
            }

            // 3. /model [name]
            "model" | "m" => {
                if let Some(new_model) = args.first() {
                    self.conversation.add_user_message(raw_cmd);
                    let old = self.model.selection_id();
                    self.model = ModelRef::parse(new_model);
                    let out = format!("✔ Model switched from `{old}` to **`{new_model}`**.");
                    self.conversation.add_assistant_message(Some(out), None);
                    self.set_status_message(format!("Switched model to {}", new_model));
                    self.save_and_refresh();
                } else {
                    self.spawn_model_picker(event_tx.clone());
                }
                true
            }

            // 4. /mode [auto | single]
            "mode" | "topology" => {
                if let Some(m_str) = args.first() {
                    self.conversation.add_user_message(raw_cmd);
                    let new_mode = match m_str.to_lowercase().as_str() {
                        "auto" | "router" => Some(ExecutionMode::AutoRouter),
                        "single" | "direct" => Some(ExecutionMode::SingleAgent),
                        _ => None,
                    };

                    if let Some(m) = new_mode {
                        self.execution_mode = m;
                        let out = format!(
                            "✔ Execution mode switched to **`{}`** ({})\n",
                            self.execution_mode.badge(),
                            self.execution_mode.description()
                        );
                        self.conversation.add_assistant_message(Some(out), None);
                        self.set_status_message(format!(
                            "Mode: {}",
                            self.execution_mode.description()
                        ));
                    } else {
                        let out = format!("❌ Unknown mode `{m_str}`. Available: `auto`, `single`");
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                    self.save_and_refresh();
                } else {
                    let items = vec![
                        PickerItem::new(
                            "auto",
                            "⚡ Auto (Plugin Host)",
                            "ThunderRoot Microkernel Host (Conversation + Skills + MCP)",
                        )
                        .with_badge("Recommended"),
                        PickerItem::new(
                            "single",
                            "Single Agent",
                            "Single Agent Direct Run without Plugin Host",
                        )
                        .with_badge("Direct"),
                    ];
                    self.picker.open(
                        PickerKind::SelectMode,
                        Some("⚡ Select Execution Mode (↑/↓ to move, Enter to select)".to_string()),
                        items,
                    );
                }
                true
            }

            // 5. /skills [list | load <name> | scan [path]]
            "skills" | "skill" | "sk" => {
                let sub_action = args.first().copied().unwrap_or("picker");

                match sub_action {
                    "off" | "detach" | "clear" | "none" => {
                        self.conversation.add_user_message(raw_cmd);
                        self.detach_skill_handler();
                    }
                    "load" => {
                        if let Some(skill_name) = args.get(1) {
                            self.attach_skill_handler(skill_name, "Attached from /skills load");
                        } else {
                            self.spawn_skills_picker(event_tx.clone());
                        }
                    }
                    "show" | "view" => {
                        if let Some(skill_name) = args.get(1) {
                            self.conversation.add_user_message(raw_cmd);
                            let s_name = skill_name.to_string();
                            let default_paths =
                                thunder_agent_skills::loader::SkillLoader::default_search_paths();

                            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                            tokio::spawn(async move {
                                let skills =
                                    thunder_agent_skills::loader::SkillLoader::load_search_paths(
                                        &default_paths,
                                    )
                                    .await;
                                if let Some(found) = skills
                                    .into_iter()
                                    .find(|s| s.name.eq_ignore_ascii_case(&s_name))
                                {
                                    let out = thunder_agent_skills::SkillRegistry::new();
                                    let _ = out.register(found.clone()).await;
                                    let rendered = out
                                        .render_skill_markdown(&found.name)
                                        .await
                                        .unwrap_or_else(|| found.prompt_instructions.clone());
                                    let _ = tx.send(rendered);
                                } else {
                                    let _ = tx.send(format!("❌ Skill `{s_name}` not found. Use `/skills` to browse available skills."));
                                }
                            });

                            let event_tx_clone = event_tx.clone();
                            tokio::spawn(async move {
                                if let Some(content) = rx.recv().await {
                                    let _ = event_tx_clone.send(
                                        crate::event::AppEvent::AgentFinished {
                                            agent_id: "skills_show".to_string(),
                                            success: true,
                                            final_text: Some(content),
                                            authoritative_messages: None,
                                            raw_messages: None,
                                        },
                                    );
                                }
                            });
                        } else {
                            self.spawn_skills_picker(event_tx.clone());
                        }
                    }
                    "scan" | "reload" => {
                        self.conversation.add_user_message(raw_cmd);
                        let custom_path = args.get(1).map(PathBuf::from);
                        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                        tokio::spawn(async move {
                            if let Some(p) = custom_path {
                                let mut paths =
                                    thunder_agent_skills::loader::SkillLoader::default_search_paths(
                                    );
                                paths.push(p);
                                let loaded =
                                    thunder_agent_skills::loader::SkillLoader::load_search_paths(
                                        &paths,
                                    )
                                    .await;
                                let report = format!(
                                    "✔ Scanned and indexed **{}** skill(s) including custom path.",
                                    loaded.len()
                                );
                                let _ = tx.send(report);
                            } else {
                                let (_, report) = thunder_agent_skills::loader::SkillLoader::scan_default_and_report().await;
                                let _ = tx.send(report);
                            }
                        });

                        let event_tx_clone = event_tx.clone();
                        tokio::spawn(async move {
                            if let Some(content) = rx.recv().await {
                                let _ =
                                    event_tx_clone.send(crate::event::AppEvent::AgentFinished {
                                        agent_id: "skills_scanner".to_string(),
                                        success: true,
                                        final_text: Some(content),
                                        authoritative_messages: None,
                                        raw_messages: None,
                                    });
                            }
                        });
                    }
                    "list" => {
                        self.conversation.add_user_message(raw_cmd);
                        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                        tokio::spawn(async move {
                            let (skills, _) =
                                thunder_agent_skills::loader::SkillLoader::scan_default_and_report(
                                )
                                .await;
                            let out = thunder_agent_skills::registry::SkillRegistry::format_skills_catalog_markdown(&skills);
                            let _ = tx.send(out);
                        });

                        let event_tx_clone = event_tx.clone();
                        tokio::spawn(async move {
                            if let Some(content) = rx.recv().await {
                                let _ =
                                    event_tx_clone.send(crate::event::AppEvent::AgentFinished {
                                        agent_id: "skills_lister".to_string(),
                                        success: true,
                                        final_text: Some(content),
                                        authoritative_messages: None,
                                        raw_messages: None,
                                    });
                            }
                        });
                    }
                    _ => {
                        self.spawn_skills_picker(event_tx.clone());
                    }
                }
                true
            }

            // 6. /mcp [list | servers | reload]
            "mcp" | "server" | "tools" => {
                let sub = args.first().copied().unwrap_or("picker");
                if sub == "list" || sub == "servers" || sub == "reload" {
                    self.conversation.add_user_message(raw_cmd);
                    let ws = self.workspace_dir.clone();

                    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                    tokio::spawn(async move {
                        if let Some((path, cfg)) =
                            thunder_agent_mcp::config::McpConfig::find_and_load_from_workspace(&ws)
                                .await
                        {
                            let out = cfg.render_servers_markdown(Some(&path));
                            let _ = tx.send(out);
                        } else {
                            let out = "### 🔌 Model Context Protocol (MCP)\n\nNo `mcp_servers.json` or `.mcp.json` found in workspace.\n\n**Example `mcp_servers.json`:**\n```json\n{\n  \"mcpServers\": {\n    \"filesystem\": {\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]\n    }\n  }\n}\n```".to_string();
                            let _ = tx.send(out);
                        }
                    });

                    let event_tx_clone = event_tx.clone();
                    tokio::spawn(async move {
                        if let Some(content) = rx.recv().await {
                            let _ = event_tx_clone.send(crate::event::AppEvent::AgentFinished {
                                agent_id: "mcp_manager".to_string(),
                                success: true,
                                final_text: Some(content),
                                authoritative_messages: None,
                                raw_messages: None,
                            });
                        }
                    });
                } else {
                    self.spawn_mcp_picker(event_tx.clone());
                }
                true
            }

            // 6. /clear or /new or /reset
            "clear" | "new" | "reset" => {
                self.new_session();
                self.conversation.add_assistant_message(
                    Some(
                        "✨ Started a fresh conversation session. How can I help you today?"
                            .to_string(),
                    ),
                    None,
                );
                self.save_and_refresh();
                true
            }

            // 7. /config [key] [val]
            "config" | "settings" | "cfg" => {
                self.conversation.add_user_message(raw_cmd);
                if args.len() >= 2 {
                    let key = args[0].to_lowercase();
                    let val = args[1];
                    match key.as_str() {
                        "temperature" | "temp" => {
                            if let Ok(t) = val.parse::<f32>() {
                                self.temperature = t;
                                self.conversation.add_assistant_message(
                                    Some(format!("✔ `temperature` set to `{t}`")),
                                    None,
                                );
                            }
                        }
                        "max_turns" | "turns" => {
                            if let Ok(turns) = val.parse::<usize>() {
                                self.max_turns = turns;
                                self.conversation.add_assistant_message(
                                    Some(format!("✔ `max_turns` set to `{turns}`")),
                                    None,
                                );
                            }
                        }
                        "timeout" | "timeout_ms" => {
                            if let Ok(ms) = val.parse::<u64>() {
                                self.request_timeout_ms = ms;
                                self.conversation.add_assistant_message(
                                    Some(format!("✔ `request_timeout_ms` set to `{ms}`")),
                                    None,
                                );
                            }
                        }
                        "model" => {
                            self.model = ModelRef::parse(val);
                            self.conversation.add_assistant_message(
                                Some(format!("✔ `model` set to `{val}`")),
                                None,
                            );
                        }
                        other => {
                            self.conversation.add_assistant_message(Some(format!("❌ Unknown config key `{other}`. Available: `model`, `temperature`, `max_turns`, `timeout_ms`")), None);
                        }
                    }
                } else {
                    let mut out = String::from("### ⚙️ Thunder Runtime Configuration\n\n");
                    out.push_str("| Parameter | Value | Description |\n");
                    out.push_str("|---|---|---|\n");
                    out.push_str(&format!(
                        "| `model` | `{}` | Active LLM model |\n",
                        self.model.selection_id()
                    ));
                    out.push_str(&format!(
                        "| `mode` | `{:?}` | Execution mode |\n",
                        self.execution_mode
                    ));
                    out.push_str(&format!(
                        "| `workspace` | `{}` | Working directory |\n",
                        self.workspace_dir.display()
                    ));
                    out.push_str(&format!(
                        "| `temperature` | `{}` | Sampling temperature |\n",
                        self.temperature
                    ));
                    out.push_str(&format!(
                        "| `max_turns` | `{}` | Loop turn guard limit |\n",
                        self.max_turns
                    ));
                    out.push_str(&format!(
                        "| `timeout_ms` | `{} ms` | Request timeout |\n",
                        self.request_timeout_ms
                    ));
                    out.push_str("\n*Update values with `/config <key> <value>` (e.g. `/config temperature 0.7`)*");
                    self.conversation.add_assistant_message(Some(out), None);
                }
                self.save_and_refresh();
                true
            }

            // 8. /compact or /prune or /compress
            "compact" | "prune" | "compress" => {
                self.conversation.add_user_message(raw_cmd);
                let (before, after) = self.conversation.compact_history(4);
                let out = if before > after {
                    format!("✔ Context compacted: compressed from {before} messages down to {after} messages.")
                } else {
                    format!("Context is already compact ({before} messages). No pruning needed.")
                };
                self.conversation.add_assistant_message(Some(out), None);
                self.save_and_refresh();
                true
            }

            // 9. /stats or /cost or /usage
            "stats" | "cost" | "tokens" | "usage" => {
                self.conversation.add_user_message(raw_cmd);
                let out = self.conversation.format_stats_markdown();
                self.conversation.add_assistant_message(Some(out), None);
                self.save_and_refresh();
                true
            }

            // 10. /workspace [path] or /cwd
            "workspace" | "cwd" | "dir" => {
                self.conversation.add_user_message(raw_cmd);
                if let Some(new_path) = args.first() {
                    let pb = PathBuf::from(new_path);
                    if pb.is_dir() {
                        self.workspace_dir = pb.clone();
                        let out = format!("✔ Workspace switched to: `{}`", pb.display());
                        self.conversation.add_assistant_message(Some(out), None);
                        self.set_status_message(format!("Workspace: {}", pb.display()));
                    } else {
                        let out = format!("❌ Directory `{}` does not exist.", pb.display());
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                } else {
                    let out = format!("### 📂 Current Workspace Directory\n\n`{}`\n\n*Change with `/workspace <path>`*", self.workspace_dir.display());
                    self.conversation.add_assistant_message(Some(out), None);
                }
                self.save_and_refresh();
                true
            }

            // 11. /export [path] or /save
            "export" | "dump" => {
                self.conversation.add_user_message(raw_cmd);
                let target_path = args.first().map(PathBuf::from).unwrap_or_else(|| {
                    PathBuf::from(format!("thunder_session_{}.md", self.conversation.id))
                });

                let doc = ConversationExporter::to_markdown(&self.conversation);

                if let Err(e) = std::fs::write(&target_path, doc) {
                    let out = format!(
                        "❌ Failed to export conversation to `{}`: {e}",
                        target_path.display()
                    );
                    self.conversation.add_assistant_message(Some(out), None);
                } else {
                    let out = format!(
                        "✔ Conversation successfully exported to: **`{}`**",
                        target_path.display()
                    );
                    self.conversation.add_assistant_message(Some(out), None);
                    self.set_status_message(format!("Exported to {}", target_path.display()));
                }
                self.save_and_refresh();
                true
            }

            // 12. /health or /doctor — lightweight local diagnostics (no LLM call)
            "health" | "doctor" => {
                self.conversation.add_user_message(raw_cmd);
                let model = self.model.selection_id();
                let spec = self.provider_registry.resolve(&model);
                let mut out = String::from("### 🩺 Thunder Health Report\n\n");
                let model_line = match &spec {
                    Some(s) => format!("- ✔ Model `{}` resolves (provider: {}, context window: {})", model, s.provider, s.context_window),
                    None => format!("- ❌ Model `{}` not found in provider registry (~/.thunder/models.json + auth.json)", model),
                };
                out.push_str(&model_line);
                out.push_str(&format!(
                    "\n- {} Workspace: `{}`",
                    if self.workspace_dir.is_dir() {
                        "✔"
                    } else {
                        "❌"
                    },
                    self.workspace_dir.display()
                ));
                out.push_str(&format!(
                    "\n- {} Session `{}` with {} messages",
                    "✔",
                    self.conversation.id,
                    self.conversation.messages.len()
                ));
                out.push_str(&format!("\n- Mode: {}", self.execution_mode.description()));
                if spec.is_none() {
                    out.push_str("\n\n⚠️ LIVE mode will fail at the first LLM call until the model is configured.");
                }
                self.conversation.add_assistant_message(Some(out), None);
                self.save_and_refresh();
                true
            }

            // 13. /quit or /exit
            "quit" | "exit" | "q" => {
                self.should_quit = true;
                true
            }

            _ => false,
        }
    }

    pub fn submit_prompt(
        &mut self,
        raw_prompt: String,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let trimmed = raw_prompt.trim();

        let (mode, prompt) = if let Some(p) = trimmed.strip_prefix("/single ") {
            (ExecutionMode::SingleAgent, p.to_string())
        } else if let Some(p) = trimmed.strip_prefix("/auto ") {
            (ExecutionMode::AutoRouter, p.to_string())
        } else {
            (self.execution_mode, raw_prompt)
        };

        self.conversation.add_user_message(prompt.clone());
        self.agent_status = AgentStatus::Thinking;
        self.streaming_delta.clear();
        self.reasoning_delta.clear();
        self.active_tool_calls.clear();
        self.last_error = None;
        self.auto_scroll = true;

        self.save_and_refresh();

        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());

        match mode {
            ExecutionMode::SingleAgent => {
                self.run_single_agent(prompt, cancel, event_tx);
            }
            ExecutionMode::AutoRouter => {
                self.run_root_agent(prompt, cancel, event_tx);
            }
        }
    }

    fn run_root_agent(
        &self,
        _prompt: String,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let model = self.model.selection_id();
        let timeout_ms = self.request_timeout_ms;
        let mut base_cfg = AgentConfig::new(model.clone()).with_unlimited_turns();
        base_cfg.temperature = Some(self.temperature);
        base_cfg.request_timeout_ms = timeout_ms;
        if let Some(spec) = self.provider_registry.resolve(&model) {
            base_cfg.pruning.max_context_tokens = spec.context_window;
        }

        let mut skills_plugin = SkillsPlugin::default();
        if let Some(handle) = &self.active_skill {
            skills_plugin = skills_plugin.with_skill(thunder_agent_skills::Skill::new(
                handle.name.clone(),
                handle.description.clone(),
                handle.system_prompt_fragment(),
            ));
        }

        let root = ThunderRoot::new(base_cfg.clone())
            .with_workspace(self.workspace_dir.clone())
            .with_plugin(ConversationPlugin::with_memory_store())
            .with_plugin(skills_plugin)
            .with_plugin(McpPlugin::default())
            .with_provider_registry(self.provider_registry.clone());

        let session_id = self.conversation.id.clone();
        let context_input = self.conversation.as_context_input();
        let factory_client = self.client_factory.as_ref().and_then(|f| f(&base_cfg));

        tokio::spawn(async move {
            let options = RootRunOptions {
                session_id: Some(session_id),
                custom_client: factory_client,
                cancellation_token: Some(cancel),
                forced_plugins: None,
                register_builtins: true,
                thinking_level: None,
                role: None,
                permission: thunder_agent_loop::types::config::Permission::default(),
                pause_gate: None,
            };

            match root.execute(context_input, options).await {
                Ok(mut handle) => {
                    let selection = handle.selection.clone();
                    if let Some(mut rx) = handle.take_events() {
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            while let Some(observed) = rx.recv().await {
                                if tx.send(crate::event::AppEvent::Agent(observed)).is_err() {
                                    break;
                                }
                            }
                        });
                    }

                    match handle.join().await {
                        Ok(res) => {
                            let mut summary = String::new();
                            if !selection.active_plugin_ids.is_empty() {
                                summary.push_str(&format!(
                                    "> ⚡ **Root Active Plugins**: `{:?}` (Reason: {})\n\n",
                                    selection.active_plugin_ids, selection.reason
                                ));
                            }
                            if let Some(content) = &res.final_content {
                                summary.push_str(content);
                            }

                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: res.agent_id,
                                success: res.run_result.finish_reason == FinishReason::Done,
                                final_text: Some(summary),
                                authoritative_messages: Some(res.run_result.messages),
                                raw_messages: res.run_result.raw_messages,
                            });
                        }
                        Err(err) => {
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: "root_agent".to_string(),
                                success: false,
                                final_text: Some(err.to_string()),
                                authoritative_messages: None,
                                raw_messages: None,
                            });
                        }
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                        agent_id: "root_agent".to_string(),
                        success: false,
                        final_text: Some(err.to_string()),
                        authoritative_messages: None,
                        raw_messages: None,
                    });
                }
            }
        });
    }

    fn run_single_agent(
        &self,
        _prompt: String,
        cancel: CancellationToken,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let model = self.model.selection_id();
        let timeout_ms = self.request_timeout_ms;
        let mut config = AgentConfig::new(model.clone()).with_unlimited_turns();
        config.temperature = Some(self.temperature);
        config.request_timeout_ms = timeout_ms;
        if let Some(handle) = &self.active_skill {
            let fragment = handle.system_prompt_fragment();
            config.system_prompt = Some(match config.system_prompt.take() {
                Some(existing) => format!("{existing}\n\n{fragment}"),
                None => fragment,
            });
        }

        let context_input = self.conversation.as_context_input();
        let factory_client = self.client_factory.as_ref().and_then(|f| f(&config));

        tokio::spawn(async move {
            let mut agent = AgentLoop::new(config.clone()).with_id("tui_agent");

            // Resolve the transport: injected factory (tests/embedders) first,
            // then the provider registry. Never proceed without a client — a
            // silent `UnconfiguredLLMClient` run only fails later with a
            // confusing error.
            let client = match factory_client {
                Some(client) => Some(client),
                None => match client_for_selection(&model, timeout_ms).await {
                    Ok((_spec, client)) => Some(client),
                    Err(err) => {
                        let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                            agent_id: "tui_agent".to_string(),
                            success: false,
                            final_text: Some(format!(
                                "❌ No LLM client available for model `{model}`: {err}\n\n\
                                 Configure it in ~/.thunder/models.json + auth.json, switch \
                                 with `/model <id>`, or inject a client via \
                                 `App::with_client_factory`."
                            )),
                            authoritative_messages: None,
                            raw_messages: None,
                        });
                        return;
                    }
                },
            };
            if let Some(client) = client {
                agent = agent.with_custom_client(client);
            }
            agent.register_tool(Arc::new(BashTool::default()));
            agent.register_tool(Arc::new(ReadFileTool::default()));
            agent.register_tool(Arc::new(WriteFileTool::default()));

            match agent.start(context_input, Some(cancel)) {
                Ok(mut handle) => {
                    if let Some(mut rx) = handle.take_events() {
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            while let Some(observed) = rx.recv().await {
                                if tx.send(crate::event::AppEvent::Agent(observed)).is_err() {
                                    break;
                                }
                            }
                        });
                    }

                    match handle.join().await {
                        Ok(result) => {
                            let is_ok = result.finish_reason == FinishReason::Done;
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: result.agent_id,
                                success: is_ok,
                                final_text: result.final_content,
                                authoritative_messages: Some(result.messages),
                                raw_messages: result.raw_messages,
                            });
                        }
                        Err(err) => {
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: "tui_agent".to_string(),
                                success: false,
                                final_text: Some(err.to_string()),
                                authoritative_messages: None,
                                raw_messages: None,
                            });
                        }
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                        agent_id: "tui_agent".to_string(),
                        success: false,
                        final_text: Some(err.to_string()),
                        authoritative_messages: None,
                        raw_messages: None,
                    });
                }
            }
        });
    }

    pub fn handle_agent_event(&mut self, event: ObservedEvent) {
        match event.event {
            AgentEvent::TurnStart { turn, .. } => {
                self.agent_status = AgentStatus::Thinking;
                info!("[{}] Turn {} started", event.agent_id, turn);
            }
            AgentEvent::TokenDelta { delta, .. } => {
                self.agent_status = AgentStatus::Streaming;
                self.streaming_delta.push_str(&delta);
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                self.reasoning_delta.push_str(&delta);
            }
            AgentEvent::ToolCallReady { tool_call, .. } => {
                let call_id = tool_call.id.clone();
                let already_committed = self.conversation.messages.iter().rev().any(|message| {
                    matches!(
                        message,
                        ChatMessage::Assistant {
                            tool_calls: Some(calls),
                            ..
                        } if calls.iter().any(|call| call.id == call_id)
                    )
                });

                if !already_committed {
                    self.conversation
                        .add_assistant_message(None, Some(vec![tool_call.clone()]));
                }

                self.active_tool_calls.push(ActiveToolCall {
                    id: tool_call.id.clone(),
                    name: tool_call.function.name.clone(),
                    arguments: tool_call.function.arguments.clone(),
                    result: None,
                    is_error: false,
                    duration_ms: 0,
                });
            }
            AgentEvent::ToolExecStart { name, .. } => {
                self.agent_status = AgentStatus::ExecutingTool {
                    name,
                    duration_ms: 0,
                };
            }
            AgentEvent::ToolExecResult {
                tool_call_id,
                name,
                result,
                ..
            } => {
                if let Some(tc) = self
                    .active_tool_calls
                    .iter_mut()
                    .find(|c| c.id == tool_call_id)
                {
                    tc.result = Some(result.output.clone());
                    tc.is_error = result.is_error;
                    tc.duration_ms = result.duration_ms;
                }

                self.conversation
                    .add_tool_message(&tool_call_id, result.output, Some(name));
                self.active_tool_calls.retain(|c| c.id != tool_call_id);
                self.agent_status = AgentStatus::Thinking;
            }
            AgentEvent::TurnEnd { .. } => {
                if !self.streaming_delta.is_empty() {
                    let content = std::mem::take(&mut self.streaming_delta);
                    self.conversation.add_assistant_message(Some(content), None);
                }

                self.reasoning_delta.clear();
                self.streaming_delta.clear();
            }
            AgentEvent::Error { message, .. } => {
                self.last_error = Some(message.clone());
                self.agent_status = AgentStatus::Error(message.clone());
                self.set_status_message(format!("Error: {}", message));
            }
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                ..
            } => {
                info!(
                    "[{}] context compacted: {} -> {} estimated tokens",
                    event.agent_id, tokens_before, tokens_after
                );
                self.set_status_message(format!(
                    "🗜 Context compacted: ~{} → ~{} tokens (older history summarized)",
                    tokens_before, tokens_after
                ));
            }
            _ => {}
        }
    }

    pub fn handle_agent_finished(
        &mut self,
        _agent_id: String,
        success: bool,
        final_text: Option<String>,
        authoritative_messages: Option<Vec<ChatMessage>>,
        raw_messages: Option<Vec<ChatMessage>>,
    ) {
        // Preserve the raw pre-compaction transcript (if a checkpoint compaction
        // fired) so the working history can be a checkpoint projection while the
        // original history stays auditable on disk.
        self.pending_raw_transcript = raw_messages;
        if success {
            self.agent_status = AgentStatus::Idle;
            if let Some(messages) = authoritative_messages {
                if !messages.is_empty() {
                    self.conversation.messages = messages;
                    self.conversation.recalculate_stats();
                }
            } else if !self.streaming_delta.is_empty() {
                let content = std::mem::take(&mut self.streaming_delta);
                let tool_calls: Option<Vec<ToolCall>> = if !self.active_tool_calls.is_empty() {
                    Some(
                        self.active_tool_calls
                            .iter()
                            .map(|c| ToolCall::new_function(&c.id, &c.name, &c.arguments))
                            .collect(),
                    )
                } else {
                    None
                };

                self.conversation
                    .add_assistant_message(Some(content), tool_calls);

                for tc in &self.active_tool_calls {
                    if let Some(res) = &tc.result {
                        self.conversation
                            .add_tool_message(&tc.id, res, Some(tc.name.clone()));
                    }
                }
            } else if let Some(final_content) = final_text {
                let already_present = self
                    .conversation
                    .messages
                    .last()
                    .map(|m| match m {
                        ChatMessage::Assistant {
                            content: Some(c), ..
                        } => c == &final_content,
                        _ => false,
                    })
                    .unwrap_or(false);

                if !already_present && !final_content.is_empty() {
                    self.conversation
                        .add_assistant_message(Some(final_content), None);
                }
            }
            self.last_error = None;
        } else {
            let err_msg = self.last_error.take().or_else(|| {
                if !self.streaming_delta.is_empty() {
                    Some(std::mem::take(&mut self.streaming_delta))
                } else {
                    final_text
                }
            }).unwrap_or_else(|| "Agent execution failed. No available LLM provider or the request was interrupted.".to_string());

            self.agent_status = AgentStatus::Error(err_msg.clone());
            self.conversation
                .add_assistant_message(Some(format!("❌ {}", err_msg)), None);
            self.set_status_message(format!("Error: {}", err_msg));
        }

        self.streaming_delta.clear();
        self.reasoning_delta.clear();
        self.active_tool_calls.clear();
        self.cancel_token = None;
    }
}

fn fallback_model_items() -> Vec<PickerItem> {
    vec![
        PickerItem::new("openai/gpt-4o", "GPT-4o", "openai-completions").with_badge("openai"),
        PickerItem::new("openai/gpt-4o-mini", "GPT-4o Mini", "openai-completions")
            .with_badge("openai"),
        PickerItem::new(
            "anthropic/claude-3-7-sonnet-latest",
            "Claude 3.7 Sonnet",
            "anthropic-messages",
        )
        .with_badge("anthropic"),
        PickerItem::new(
            "deepseek/deepseek-chat",
            "DeepSeek V3",
            "openai-completions",
        )
        .with_badge("deepseek"),
    ]
}
