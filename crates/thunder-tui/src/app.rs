use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
}

impl PickerItem {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        subtitle: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            badge: None,
        }
    }

    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Model,
    ResumeSession,
    PromptTemplate,
    Tools,
}

#[derive(Debug, Clone, Default)]
pub struct PickerState {
    pub is_open: bool,
    pub kind: Option<PickerKind>,
    pub title: String,
    pub filter_text: String,
    pub selected_index: usize,
    pub items: Vec<PickerItem>,
}

impl PickerState {
    pub fn open(&mut self, kind: PickerKind, title: impl Into<String>, items: Vec<PickerItem>) {
        self.is_open = true;
        self.kind = Some(kind);
        self.title = title.into();
        self.filter_text.clear();
        self.selected_index = 0;
        self.items = items;
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.kind = None;
        self.filter_text.clear();
        self.selected_index = 0;
        self.items.clear();
    }

    pub fn filtered_items(&self) -> Vec<&PickerItem> {
        let needle = self.filter_text.trim().to_lowercase();
        if needle.is_empty() {
            return self.items.iter().collect();
        }
        self.items
            .iter()
            .filter(|item| {
                item.title.to_lowercase().contains(&needle)
                    || item.id.to_lowercase().contains(&needle)
                    || item.subtitle.to_lowercase().contains(&needle)
                    || item
                        .badge
                        .as_deref()
                        .map(|b| b.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .collect()
    }

    pub fn move_selection_up(&mut self) {
        let len = self.filtered_items().len();
        if len > 0 {
            if self.selected_index == 0 {
                self.selected_index = len - 1;
            } else {
                self.selected_index -= 1;
            }
        }
    }

    pub fn move_selection_down(&mut self) {
        let len = self.filtered_items().len();
        if len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        }
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        let filtered = self.filtered_items();
        if filtered.is_empty() {
            None
        } else {
            let idx = self.selected_index.min(filtered.len() - 1);
            Some(filtered[idx])
        }
    }

    pub fn handle_char(&mut self, ch: char) {
        self.filter_text.push(ch);
        self.selected_index = 0;
    }

    pub fn handle_backspace(&mut self) {
        self.filter_text.pop();
        self.selected_index = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommandDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args_hint: &'static str,
    pub description: &'static str,
    pub arg_options: &'static [&'static str],
}

pub static ALL_SLASH_COMMANDS: &[SlashCommandDef] = &[
    SlashCommandDef {
        name: "model",
        aliases: &["m"],
        args_hint: "[id]",
        description: "Open interactive model picker modal or switch model",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "think",
        aliases: &["thinking"],
        args_hint: "[off | low | medium | high | max]",
        description: "View or set reasoning effort bound to this session",
        arg_options: &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
    },
    SlashCommandDef {
        name: "mode",
        aliases: &[],
        args_hint: "[yolo | plan]",
        description: "Toggle or set permission mode (Shift+Tab to toggle)",
        arg_options: &["yolo", "plan", "ask", "acceptEdits"],
    },
    SlashCommandDef {
        name: "resume",
        aliases: &["switch", "load", "sessions"],
        args_hint: "[id | #]",
        description: "Open interactive session picker modal or resume by ID/index",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "prompt",
        aliases: &["template", "prompts"],
        args_hint: "[name]",
        description: "Open prompt templates picker or load template into input",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "tools",
        aliases: &["mcp"],
        args_hint: "",
        description: "Open tool catalog modal showing core, MCP, and script tools",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "compact",
        aliases: &["prune", "compress"],
        args_hint: "",
        description: "Compact conversation history and prune tokens via checkpoint",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "stats",
        aliases: &["tokens", "usage", "cost"],
        args_hint: "",
        description: "Display dialogue turns, token estimations, and tool stats",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "new",
        aliases: &[],
        args_hint: "",
        description: "Start a fresh conversation session",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "rename",
        aliases: &[],
        args_hint: "[title | --auto]",
        description: "Rename current session or regenerate title with --auto",
        arg_options: &["--auto"],
    },
    SlashCommandDef {
        name: "workspace",
        aliases: &["cwd", "dir"],
        args_hint: "[path]",
        description: "View or change current workspace directory",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "export",
        aliases: &["dump", "save"],
        args_hint: "[path]",
        description: "Export conversation messages to Markdown file",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "pause",
        aliases: &["hold"],
        args_hint: "",
        description: "Pause running agent at next tool boundary (Ctrl+P)",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "unpause",
        aliases: &["go", "continue"],
        args_hint: "",
        description: "Resume paused agent task execution",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "queue",
        aliases: &["pending", "queued"],
        args_hint: "[clear]",
        description: "Inspect or clear queued steering messages",
        arg_options: &["clear"],
    },
    SlashCommandDef {
        name: "health",
        aliases: &["doctor"],
        args_hint: "",
        description: "Run system health and environment diagnostics",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "image",
        aliases: &["attach", "img"],
        args_hint: "<path>",
        description: "Inspect and attach local image to next prompt",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "clear",
        aliases: &[],
        args_hint: "",
        description: "Clear timeline display",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "help",
        aliases: &["?"],
        args_hint: "",
        description: "Show interactive commands and keyboard shortcuts",
        arg_options: &[],
    },
    SlashCommandDef {
        name: "quit",
        aliases: &["exit", "q"],
        args_hint: "",
        description: "Cleanly exit Thunder TUI",
        arg_options: &[],
    },
];

pub fn filter_slash_commands(input: &str) -> Vec<&'static SlashCommandDef> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let query = input.trim_start_matches('/').to_lowercase();
    let query_token = query.split_whitespace().next().unwrap_or("");
    ALL_SLASH_COMMANDS
        .iter()
        .filter(|cmd| {
            if query_token.is_empty() {
                return true;
            }
            if cmd.name.starts_with(query_token) {
                return true;
            }
            cmd.aliases.iter().any(|a| a.starts_with(query_token))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputCompletionContext<'a> {
    Command {
        matches: Vec<&'static SlashCommandDef>,
    },
    Argument {
        command: &'static SlashCommandDef,
        prefix: &'a str,
        candidates: Vec<String>,
    },
}

pub fn resolve_completion_context<'a>(
    input: &'a str,
    app: &App,
) -> Option<InputCompletionContext<'a>> {
    if !input.starts_with('/') {
        return None;
    }
    let after_slash = &input[1..];
    if let Some((cmd_part, arg_part)) = after_slash.split_once(char::is_whitespace) {
        let cmd_token = cmd_part.to_lowercase();
        let cmd = ALL_SLASH_COMMANDS
            .iter()
            .find(|c| c.name == cmd_token || c.aliases.contains(&cmd_token.as_str()))?;
        let arg_prefix = arg_part.trim();

        // 1. Fixed command options (e.g. /think, /mode, /queue)
        if !cmd.arg_options.is_empty() {
            let candidates: Vec<String> = cmd
                .arg_options
                .iter()
                .filter(|opt| arg_prefix.is_empty() || opt.starts_with(arg_prefix))
                .map(|s| s.to_string())
                .collect();
            if !candidates.is_empty() {
                return Some(InputCompletionContext::Argument {
                    command: cmd,
                    prefix: arg_prefix,
                    candidates,
                });
            }
        }

        // 2. Dynamic model options
        if cmd.name == "model" {
            let models = app.provider_registry.list_available();
            let needle = arg_prefix.to_lowercase();
            let candidates: Vec<String> = models
                .iter()
                .map(|m| m.selection_id())
                .filter(|id| needle.is_empty() || id.to_lowercase().contains(&needle))
                .collect();
            if !candidates.is_empty() {
                return Some(InputCompletionContext::Argument {
                    command: cmd,
                    prefix: arg_prefix,
                    candidates,
                });
            }
        }

        // 3. Dynamic prompt template options
        if cmd.name == "prompt" {
            let candidates = list_prompt_names(&app.workspace_dir);
            let needle = arg_prefix.to_lowercase();
            let filtered: Vec<String> = candidates
                .into_iter()
                .filter(|p| needle.is_empty() || p.to_lowercase().contains(&needle))
                .collect();
            if !filtered.is_empty() {
                return Some(InputCompletionContext::Argument {
                    command: cmd,
                    prefix: arg_prefix,
                    candidates: filtered,
                });
            }
        }

        None
    } else {
        let matches = filter_slash_commands(input);
        if matches.is_empty() {
            None
        } else {
            Some(InputCompletionContext::Command { matches })
        }
    }
}

pub fn list_prompt_names(workspace_dir: &Path) -> Vec<String> {
    let candidates = vec![
        workspace_dir.join(".thunder").join("prompts"),
        thunder_protocol::thunder_subdir("prompts"),
    ];
    let mut names = Vec::new();
    for dir in candidates {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|ext| ext == "md") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if !names.contains(&stem.to_string()) {
                            names.push(stem.to_string());
                        }
                    }
                }
            }
        }
    }
    names.sort();
    names
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
    pub thinking_level: Option<String>,
    pub is_running: bool,
    pub is_paused: bool,
    pub active_cancel: Option<CancellationToken>,
    pub active_pause: Option<Arc<PauseGate>>,
    pub active_steer: Option<Arc<SteerQueues>>,
    pub should_quit: bool,
    pub picker: PickerState,
    pub autocomplete_index: usize,
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
                text: "Welcome to Thunder TUI. Type / to view commands or /help for guidance."
                    .to_string(),
            }],
            input: String::new(),
            scroll_offset: 0,
            status_message: "Ready".to_string(),
            estimated_tokens: 0,
            thinking_level: None,
            is_running: false,
            is_paused: false,
            active_cancel: None,
            active_pause: None,
            active_steer: None,
            should_quit: false,
            picker: PickerState::default(),
            autocomplete_index: 0,
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

    pub fn open_model_picker(&mut self) {
        let models = self.provider_registry.list_available();
        let items: Vec<PickerItem> = models
            .iter()
            .map(|m| {
                let badge = if m.available {
                    format!("{} ctx", format_tokens(m.context_window))
                } else {
                    "no-auth".to_string()
                };
                let status_label = if m.available { "Ready" } else { "Missing Auth" };
                let subtitle = format!(
                    "Provider: {} │ Status: {} │ API: {:?}",
                    m.provider, status_label, m.api
                );
                PickerItem::new(m.selection_id(), m.selection_id(), subtitle).with_badge(badge)
            })
            .collect();

        let mut final_items = items;
        if final_items.is_empty() {
            final_items.push(
                PickerItem::new(
                    self.model.clone(),
                    self.model.clone(),
                    "Active configured model",
                )
                .with_badge("Current"),
            );
        }
        self.picker
            .open(PickerKind::Model, "Select LLM Model", final_items);
    }

    pub async fn open_session_picker(&mut self) {
        if let Some(ref store) = self.store {
            let filter = ConversationFilter::new().with_min_turns(1);
            if let Ok(list) = store.list(&filter).await {
                let items: Vec<PickerItem> = list
                    .iter()
                    .map(|s| {
                        let title = s.title.as_deref().unwrap_or("Untitled");
                        let subtitle =
                            format!("Updated: {} │ {} tokens", s.updated_at_ms, s.total_tokens);
                        let badge = format!("{} turns", s.turn_count);
                        PickerItem::new(s.id.clone(), title, subtitle).with_badge(badge)
                    })
                    .collect();

                if items.is_empty() {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: "No previous saved sessions found in storage.".to_string(),
                    });
                } else {
                    self.picker.open(
                        PickerKind::ResumeSession,
                        "Resume Conversation Session",
                        items,
                    );
                }
            }
        }
    }

    pub async fn open_prompt_picker(&mut self) {
        let candidates = vec![
            self.workspace_dir.join(".thunder").join("prompts"),
            thunder_protocol::thunder_subdir("prompts"),
        ];

        let mut items = Vec::new();
        for dir in candidates {
            if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
                while let Ok(Some(e)) = entries.next_entry().await {
                    let p = e.path();
                    if p.extension().is_some_and(|ext| ext == "md") {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            let title = format!("/prompt {stem}");
                            let subtitle = format!("File: {}", p.display());
                            items.push(PickerItem::new(stem, title, subtitle).with_badge(".md"));
                        }
                    }
                }
            }
        }

        if items.is_empty() {
            self.timeline.push(TimelineItem::SystemNotice {
                text: "No prompt templates found in .thunder/prompts/ or ~/.thunder/prompts/.\nAdd templates as markdown files (e.g. .thunder/prompts/review.md).".to_string(),
            });
        } else {
            items.sort_by(|a, b| a.id.cmp(&b.id));
            self.picker
                .open(PickerKind::PromptTemplate, "Load Prompt Template", items);
        }
    }

    pub fn open_tools_picker(&mut self) {
        let snap = self.tool_registry.current();
        let mut items: Vec<PickerItem> = snap
            .descriptors
            .values()
            .map(|d| {
                let badge = if d.name.starts_with("core__") {
                    "Core"
                } else if d.name.starts_with("script__") {
                    "Script"
                } else {
                    "MCP"
                };
                PickerItem::new(d.name.clone(), d.name.clone(), d.description.clone())
                    .with_badge(badge)
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));

        self.picker.open(
            PickerKind::Tools,
            format!(
                "Registered Tools (Catalog v{}, {} total)",
                snap.version,
                items.len()
            ),
            items,
        );
    }

    pub async fn commit_picker_selection(&mut self) -> Option<String> {
        let selected = self.picker.selected_item().cloned()?;
        let kind = self.picker.kind?;
        self.picker.close();

        match kind {
            PickerKind::Model => {
                self.model = selected.id.clone();
                self.conversation.model = Some(self.model.clone());
                self.set_status_message(format!("Switched model to '{}'", self.model));
                Some(selected.id)
            }
            PickerKind::ResumeSession => {
                self.load_conversation(&selected.id).await;
                Some(selected.id)
            }
            PickerKind::PromptTemplate => {
                let candidates = vec![
                    self.workspace_dir.join(".thunder").join("prompts"),
                    thunder_protocol::thunder_subdir("prompts"),
                ];
                let file_name = format!("{}.md", selected.id);
                for dir in candidates {
                    let p = dir.join(&file_name);
                    if p.is_file() {
                        if let Ok(text) = tokio::fs::read_to_string(&p).await {
                            self.input = text.trim().to_string();
                            self.set_status_message(format!(
                                "Loaded prompt template '{}'",
                                selected.id
                            ));
                            return Some(selected.id);
                        }
                    }
                }
                None
            }
            PickerKind::Tools => {
                self.set_status_message(format!("Selected tool '{}'", selected.id));
                Some(selected.id)
            }
        }
    }

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

    pub async fn handle_slash_command(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if !trimmed.starts_with('/') {
            return false;
        }

        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().map(str::trim).unwrap_or("");

        match cmd {
            "/help" | "/?" => {
                self.timeline.push(TimelineItem::SystemNotice {
                    text: "\
⚡ Thunder Commands Reference:
  Session:
    /resume [id | #]  - Open interactive session picker or resume by ID/index (aliases: /switch, /load)
    /new              - Start a brand new conversation session
    /sessions         - Display saved sessions overview
    /rename <title>   - Rename current session (or /rename --auto)
    /compact          - Compact conversation history and prune tokens (alias: /prune)
    /stats            - Display tokens, turns, and execution stats dashboard
    /export [path]    - Export session messages to Markdown file (alias: /save)
  Configuration & Mode:
    /model [id]       - Open interactive model picker or switch model
    /think [level]    - Open thinking picker or set effort (off, low, medium, high, max)
    /mode [yolo|plan] - Toggle or set permission mode (Shift+Tab to toggle)
    /workspace [dir]  - View or change workspace directory (alias: /cwd)
    /tools            - Open interactive tools catalog modal
  Control:
    /pause, /unpause  - Pause / resume task at tool boundary (Ctrl+P)
    /queue [clear]    - View or clear pending steering messages
  Plugins & Templates:
    /prompt [name]    - Open prompt templates picker or load template
    /health           - Run system and environment health diagnostics (alias: /doctor)
    /image <path>     - Inspect and attach a local image file
  General:
    /clear            - Clear timeline view
    /quit             - Exit Thunder TUI (Ctrl+C / /exit)"
                        .to_string(),
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
                    self.open_model_picker();
                } else {
                    self.model = arg.to_string();
                    self.conversation.model = Some(self.model.clone());
                    self.set_status_message(format!("Switched model to '{}'", self.model));
                }
            }
            "/think" | "/thinking" => {
                if arg.is_empty() {
                    let levels = ["off", "low", "medium", "high", "max"];
                    let current = self.thinking_level.as_deref().unwrap_or("off");
                    let next_idx = match levels.iter().position(|l| *l == current) {
                        Some(pos) => (pos + 1) % levels.len(),
                        None => 1,
                    };
                    let next_lvl = levels[next_idx];
                    self.thinking_level = Some(next_lvl.to_string());
                    self.conversation.thinking_level = Some(next_lvl.to_string());
                    self.set_status_message(format!("Thinking effort set to '{next_lvl}'"));
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!("Thinking effort cycled to '{next_lvl}' (options: off, low, medium, high, max)"),
                    });
                } else if matches!(
                    arg.to_lowercase().as_str(),
                    "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                ) {
                    let lvl = arg.to_lowercase();
                    self.thinking_level = Some(lvl.clone());
                    self.conversation.thinking_level = Some(lvl.clone());
                    self.set_status_message(format!("Thinking effort set to '{lvl}'"));
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!("Thinking effort set to '{lvl}'"),
                    });
                } else {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!("Invalid thinking level '{arg}'. Options: off, minimal, low, medium, high, max"),
                    });
                }
            }
            "/tools" => {
                self.open_tools_picker();
            }
            "/sessions" => {
                self.open_session_picker().await;
            }
            "/resume" | "/load_session" | "/switch" | "/load" => {
                if arg.is_empty() {
                    self.open_session_picker().await;
                } else {
                    let clean_num = arg.trim_start_matches('#');
                    let mut resolved_id = arg.to_string();
                    if let Ok(idx) = clean_num.parse::<usize>() {
                        if let Some(ref store) = self.store {
                            let filter = ConversationFilter::new().with_min_turns(1);
                            if let Ok(list) = store.list(&filter).await {
                                if idx >= 1 && idx <= list.len() {
                                    resolved_id = list[idx - 1].id.clone();
                                }
                            }
                        }
                    }
                    self.load_conversation(&resolved_id).await;
                }
            }
            "/compact" | "/prune" | "/compress" => {
                let (count_before, count_after) = self.conversation.compact_history(20);
                self.estimated_tokens = self.conversation.stats.total_tokens;
                self.save_current_conversation().await;
                self.timeline.push(TimelineItem::SystemNotice {
                    text: format!(
                        "Conversation history compacted: {count_before} -> {count_after} messages (current: ~{} tokens).",
                        self.estimated_tokens
                    ),
                });
                self.set_status_message("Compacted context history");
            }
            "/stats" | "/usage" | "/tokens" | "/cost" => {
                self.conversation.recalculate_stats();
                let md = self.conversation.format_stats_markdown();
                self.timeline.push(TimelineItem::SystemNotice { text: md });
            }
            "/rename" => {
                if arg.is_empty() {
                    let title = self.conversation.title.as_deref().unwrap_or("Untitled");
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!(
                            "Current session title: '{title}'. Use `/rename <new_name>` to change."
                        ),
                    });
                } else if arg == "--auto" {
                    let first_user = self
                        .conversation
                        .messages
                        .iter()
                        .find_map(|m| {
                            if m.role() == Role::User {
                                m.content_str().map(String::from)
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| "New Conversation".to_string());
                    let new_title = thunder_core::provisional_title(&first_user);
                    self.conversation.title = Some(new_title.clone());
                    self.conversation.title_source = Some("auto".to_string());
                    self.save_current_conversation().await;
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!("Auto-regenerated title: '{new_title}'"),
                    });
                } else {
                    self.conversation.title = Some(arg.to_string());
                    self.conversation.title_source = Some("manual".to_string());
                    self.save_current_conversation().await;
                    self.set_status_message(format!("Renamed session to '{arg}'"));
                }
            }
            "/pause" | "/hold" => {
                if self.is_running {
                    if let Some(ref gate) = self.active_pause {
                        gate.pause();
                        self.is_paused = true;
                        self.set_status_message(
                            "Paused at tool boundary (Type /unpause or Ctrl+P to resume)",
                        );
                    }
                } else {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: "No active task to pause.".to_string(),
                    });
                }
            }
            "/unpause" | "/go" | "/continue" => {
                if let Some(ref gate) = self.active_pause {
                    gate.resume();
                    self.is_paused = false;
                    self.set_status_message("Resumed task execution");
                } else {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: "No paused task to resume.".to_string(),
                    });
                }
            }
            "/workspace" | "/cwd" | "/dir" => {
                if arg.is_empty() {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: format!(
                            "Current workspace directory: {}",
                            self.workspace_dir.display()
                        ),
                    });
                } else {
                    let p = PathBuf::from(arg);
                    if p.is_dir() {
                        let canonical = p.canonicalize().unwrap_or(p);
                        self.workspace_dir = canonical.clone();
                        self.conversation.workspace =
                            Some(canonical.to_string_lossy().into_owned());
                        self.set_status_message(format!(
                            "Workspace changed to {}",
                            canonical.display()
                        ));
                    } else {
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!("Directory not found: {}", p.display()),
                        });
                    }
                }
            }
            "/export" | "/dump" | "/save" => {
                let default_name = format!("thunder_session_{}.md", self.conversation.id);
                let target_path = if arg.is_empty() {
                    self.workspace_dir.join(&default_name)
                } else {
                    self.workspace_dir.join(arg)
                };

                let mut md = format!(
                    "# {}\n\n- Session ID: `{}`\n- Model: `{}`\n- Date: {}\n\n---\n\n",
                    self.conversation
                        .title
                        .as_deref()
                        .unwrap_or("Thunder Conversation"),
                    self.conversation.id,
                    self.model,
                    self.conversation.created_at_ms
                );
                for msg in &self.conversation.messages {
                    let role_str = match msg.role() {
                        Role::User => "## ❯ User",
                        Role::Assistant => "## ⚡ Thunder",
                        Role::System => "## ℹ System",
                        Role::Tool => "## ⚙ Tool",
                    };
                    md.push_str(&format!(
                        "{role_str}\n\n{}\n\n",
                        msg.content_str().unwrap_or_default()
                    ));
                }

                match tokio::fs::write(&target_path, md.as_bytes()).await {
                    Ok(_) => {
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!(
                                "Exported conversation to '{}' ({} bytes)",
                                target_path.display(),
                                md.len()
                            ),
                        });
                    }
                    Err(e) => {
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!("Failed to export conversation: {e}"),
                        });
                    }
                }
            }
            "/queue" | "/pending" | "/queued" => {
                if let Some(ref queues) = self.active_steer {
                    if arg == "clear" {
                        let (s, f) = queues.clear_all();
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!(
                                "Cleared {} steering and {} follow-up message(s) from queue.",
                                s.len(),
                                f.len()
                            ),
                        });
                    } else {
                        let snap = queues.snapshot();
                        let text = format!(
                            "Queued Messages (Total: {}):\n  Steer: {:?}\n  Follow-up: {:?}",
                            snap.total(),
                            snap.steering,
                            snap.follow_up
                        );
                        self.timeline.push(TimelineItem::SystemNotice { text });
                    }
                } else {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: "No active task or steer queue.".to_string(),
                    });
                }
            }
            "/prompt" | "/prompts" | "/template" => {
                if arg.is_empty() {
                    self.open_prompt_picker().await;
                } else {
                    let candidates = vec![
                        self.workspace_dir.join(".thunder").join("prompts"),
                        thunder_protocol::thunder_subdir("prompts"),
                    ];
                    let file_name = if arg.ends_with(".md") {
                        arg.to_string()
                    } else {
                        format!("{arg}.md")
                    };
                    let mut content_opt = None;
                    for dir in candidates {
                        let p = dir.join(&file_name);
                        if p.is_file() {
                            if let Ok(text) = tokio::fs::read_to_string(&p).await {
                                content_opt = Some((p, text));
                                break;
                            }
                        }
                    }

                    if let Some((p, text)) = content_opt {
                        self.input = text.trim().to_string();
                        self.set_status_message(format!(
                            "Loaded prompt template from {}",
                            p.display()
                        ));
                    } else {
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!(
                                "Prompt template '{arg}' not found in .thunder/prompts/."
                            ),
                        });
                    }
                }
            }
            "/health" | "/doctor" => {
                let mut report = Vec::new();
                report.push("=== Thunder System Health & Environment ===".to_string());
                report.push(format!("✓ Workspace: {}", self.workspace_dir.display()));
                report.push(format!("✓ Model Selection: {}", self.model));
                report.push(format!("✓ Mode: {}", self.mode));

                let snap = self.tool_registry.current();
                report.push(format!(
                    "✓ Registered Tools: {} (Catalog v{})",
                    snap.descriptors.len(),
                    snap.version
                ));

                let data_root = thunder_protocol::thunder_config_dir();
                report.push(format!("✓ Data Root: {}", data_root.display()));
                report.push(format!(
                    "  - models.json: {}",
                    if data_root.join("models.json").exists() {
                        "found"
                    } else {
                        "missing"
                    }
                ));
                report.push(format!(
                    "  - auth.json: {}",
                    if data_root.join("auth.json").exists() {
                        "found"
                    } else {
                        "missing"
                    }
                ));

                let prompts_dir = self.workspace_dir.join(".thunder").join("prompts");
                report.push(format!(
                    "✓ Workspace Prompts: {}",
                    if prompts_dir.is_dir() {
                        "active"
                    } else {
                        "none"
                    }
                ));
                report.push("==========================================".to_string());

                self.timeline.push(TimelineItem::SystemNotice {
                    text: report.join("\n"),
                });
            }
            "/image" | "/attach" | "/img" => {
                if arg.is_empty() {
                    self.timeline.push(TimelineItem::SystemNotice {
                        text: "Usage: /image <path_to_image> [optional prompt text]".to_string(),
                    });
                } else {
                    let mut parts = arg.splitn(2, char::is_whitespace);
                    let img_path_raw = parts.next().unwrap_or("");
                    let prompt_text = parts.next().unwrap_or("").trim();

                    let img_path = self.workspace_dir.join(img_path_raw);
                    if img_path.is_file() {
                        let size = tokio::fs::metadata(&img_path)
                            .await
                            .map(|m| m.len())
                            .unwrap_or(0);
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!(
                                "Attached image: {} ({} bytes). Prompt: '{}'",
                                img_path.display(),
                                size,
                                prompt_text
                            ),
                        });
                        if !prompt_text.is_empty() {
                            self.input = format!("Image: {} | {prompt_text}", img_path.display());
                        }
                    } else {
                        self.timeline.push(TimelineItem::SystemNotice {
                            text: format!("Image file not found: {}", img_path.display()),
                        });
                    }
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

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}
