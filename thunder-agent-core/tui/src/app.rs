use crate::ask_user::{AskUserPlugin, PendingQuestion, TuiAskUserTool};
use crate::links::{LinkCache, LinkTarget};
use crate::picker::{PickerItem, PickerKind, PickerResult, PickerState};
use crate::ui::metrics::Metrics;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
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

/// Braille spinner frames for the busy indicator.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool {
        name: String,
        duration_ms: u64,
    },
    /// A stop was requested and the run is unwinding. Distinct from `Idle` on
    /// purpose: the engine is still finishing its current step, and starting a
    /// second run on the same conversation in that window would race it.
    Stopping,
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

/// Metadata snapshot for the run whose trace is being recorded.
#[derive(Debug, Clone)]
pub struct TraceRunMeta {
    pub task_id: String,
    pub started_at_ms: u64,
    pub prompt: String,
    pub model: String,
}

pub struct App {
    pub mode: ViewMode,
    pub focus: FocusPane,
    pub execution_mode: ExecutionMode,
    pub conversation: Conversation,
    pub session_list: Vec<ConversationSummary>,
    pub selected_session_idx: usize,
    pub input: String,
    /// Caret position in the input, as a **character** index (not bytes), so
    /// multi-byte text never splits. Always clamped to the input's length.
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    /// Image attachments staged for the next prompt (`/image <path>`).
    pub pending_images: Vec<ContentPart>,
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
    /// Thinking-level override for the next run ("off" | "low" | "medium" | "high").
    /// Resolution order mirrors the daemon: manual override → conversation
    /// binding → the model's default level.
    pub thinking_level: Option<String>,
    /// Tool capability tier: the manual `/permission` tier (default: full Bash).
    pub permission: Permission,
    /// Extra workspace roots granted the same read/write standing as the
    /// primary workspace (mirrored into `conversation.shared_roots` on submit).
    pub extra_roots: Vec<PathBuf>,
    /// Whether the ask_user_question tool is mounted. Default on, matching the
    /// daemon (the Intent Gate asks when a request is ambiguous).
    pub ask_user_enabled: bool,
    /// Cooperative pause gate for the running task, if any.
    pub pause_gate: Option<Arc<PauseGate>>,
    /// Interactive question modal state; the agent is blocked while set.
    pub pending_question: Option<PendingQuestion>,
    /// Filtered event trace being recorded for the current run.
    pub trace_events: Option<Vec<ObservedEvent>>,
    /// Metadata of the run currently being traced.
    pub trace_meta: Option<TraceRunMeta>,
    /// Whether the current run is this conversation's first exchange
    /// (triggers background auto-titling once it finishes).
    pub run_is_first_exchange: bool,

    /// Render Markdown in the transcript (the default) instead of raw source.
    pub markdown_enabled: bool,
    /// Memoised "is this a real path?" answers; cleared when the roots change.
    pub link_cache: LinkCache,
    /// Clickable regions in screen coordinates, rebuilt by every render.
    pub link_hitboxes: Vec<LinkHitbox>,
    /// Targets backing the open `/links` picker, indexed by picker item id.
    pub pending_links: Vec<LinkTarget>,
    /// Advanced by every tick, so the busy spinner animates while a run is live.
    pub spinner_frame: usize,
    /// Steering / follow-up queues the live run reads from.
    pub steer_queues: Option<Arc<SteerQueues>>,
    /// Queued-but-not-yet-accepted input, kept for display. Refreshed whenever
    /// the queues change and when the engine reports one was accepted.
    pub queued: QueueSnapshot,
    /// Exact copies of what this host queued. The engine's acceptance event
    /// carries only text, so the attachments have to come from here.
    pub queued_turns: Vec<ChatMessage>,
    /// How a link is acted on. Injectable so a test can assert the whole
    /// click path without launching Finder.
    pub link_opener: Option<Arc<dyn Fn(&LinkTarget) -> std::io::Result<()> + Send + Sync>>,
    /// Token / cache / speed readout held for the metrics bar.
    pub metrics: Metrics,
}

/// A clickable region of the chat pane, in absolute terminal coordinates.
///
/// Recomputed on every frame because it depends on wrap width and scroll, so it
/// is render output rather than state anyone should persist.
#[derive(Debug, Clone)]
pub struct LinkHitbox {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
    pub target: LinkTarget,
}

impl App {
    pub fn new(model_name: impl Into<String>) -> Self {
        let model = ModelRef::parse(&model_name.into());
        let initial_conv = Conversation::new(format!("sess_{}", now_ms()))
            .with_title("New Conversation")
            .with_system_prompt(thunder_agent_loop::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT);

        Self {
            mode: ViewMode::Chat,
            focus: FocusPane::Input,
            execution_mode: ExecutionMode::AutoRouter,
            conversation: initial_conv,
            session_list: Vec::new(),
            selected_session_idx: 0,
            input: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            pending_images: Vec::new(),
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
            thinking_level: None,
            permission: Permission::default(),
            extra_roots: Vec::new(),
            ask_user_enabled: true,
            pause_gate: None,
            pending_question: None,
            trace_events: None,
            trace_meta: None,
            run_is_first_exchange: false,
            markdown_enabled: true,
            link_cache: LinkCache::new(),
            link_hitboxes: Vec::new(),
            pending_links: Vec::new(),
            spinner_frame: 0,
            steer_queues: None,
            queued: QueueSnapshot::default(),
            queued_turns: Vec::new(),
            link_opener: None,
            metrics: Metrics::new(),
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
                self.clear_link_cache();
                self.metrics.reset_for_session();
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

    // ── Input line editing ────────────────────────────────────────────────

    /// Number of characters in the input (not bytes).
    fn input_char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Byte offset of character index `idx` (clamped past the end).
    fn input_byte_offset(&self, idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(idx)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input.len())
    }

    /// Replace the input with `text`, parking the caret at the end. Every
    /// programmatic write goes through here so the caret can never go stale.
    pub fn set_input(&mut self, text: String) {
        self.input = text;
        self.input_cursor = self.input_char_len();
    }

    /// Clear the input and park the caret at the start.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }

    fn insert_char(&mut self, c: char) {
        let at = self.input_byte_offset(self.input_cursor);
        self.input.insert(at, c);
        self.input_cursor += 1;
    }

    /// Delete the character before the caret (Backspace).
    fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = self.input_byte_offset(self.input_cursor);
        let start = self.input_byte_offset(self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
    }

    /// Delete the character after the caret (Delete / forward-delete).
    fn delete_forward(&mut self) {
        if self.input_cursor >= self.input_char_len() {
            return;
        }
        let start = self.input_byte_offset(self.input_cursor);
        let end = self.input_byte_offset(self.input_cursor + 1);
        self.input.replace_range(start..end, "");
    }

    pub fn cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.input_cursor = self.input_char_len();
    }

    pub fn cursor_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        if self.input_cursor < self.input_char_len() {
            self.input_cursor += 1;
        }
    }

    /// Start of the previous word: skip spaces back, then the word's characters.
    pub fn cursor_prev_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.input_cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.input_cursor = i;
    }

    /// Start of the next word: skip the word, then the spaces after it.
    pub fn cursor_next_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.input_cursor.min(chars.len());
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.input_cursor = i;
    }

    // ── Steering / follow-up queue ───────────────────────────────────────

    /// Queue the current input into the live run.
    ///
    /// The run picks it up at the next turn boundary: steering before the next
    /// model request (and able to keep a concluding run alive), follow-up only
    /// once nothing else is left to do. Neither interrupts a running tool.
    fn enqueue_for_run(&mut self, behavior: QueueBehavior) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(queues) = self.steer_queues.as_ref() else {
            return;
        };
        // Staged images ride along, exactly as they would with a normally
        // submitted turn: a steer about a screenshot is the common case.
        let images = std::mem::take(&mut self.pending_images);
        let image_count = images.len();
        let message = ChatMessage::user_multimodal(text.clone(), images);
        match behavior {
            QueueBehavior::Steer => queues.steering.enqueue(message.clone()),
            QueueBehavior::FollowUp => queues.follow_up.enqueue(message.clone()),
        }
        self.queued_turns.push(message);
        self.queued = queues.snapshot();
        self.input_history.push(text);
        self.history_idx = None;
        self.clear_input();
        let label = match behavior {
            QueueBehavior::Steer => "Steering queued",
            QueueBehavior::FollowUp => "Follow-up queued",
        };
        let with_images = if image_count > 0 {
            format!(" with {image_count} image(s)")
        } else {
            String::new()
        };
        self.set_status_message(format!(
            "{label}{with_images} ({} pending)",
            self.queued.total()
        ));
    }

    /// Empty both queues and return the text to the editor, so cancelling a run
    /// never loses what was typed while waiting. Returns how many were restored.
    pub fn restore_queue_to_input(&mut self) -> usize {
        let Some(queues) = self.steer_queues.take() else {
            return 0;
        };
        let (steering, follow_up) = queues.clear_all();
        self.queued = QueueSnapshot::default();
        self.queued_turns.clear();

        let queued: Vec<String> = steering
            .into_iter()
            .chain(follow_up)
            .filter_map(|m| m.content_str().map(str::to_string))
            .collect();
        if queued.is_empty() {
            return 0;
        }

        let existing = std::mem::take(&mut self.input);
        let combined = if existing.trim().is_empty() {
            queued.join("\n\n")
        } else {
            format!("{}\n\n{existing}", queued.join("\n\n"))
        };
        let count = queued.len();
        self.set_input(combined);
        count
    }

    /// Re-read the queues for display. Called after the engine accepts one, and
    /// whenever the host itself enqueues.
    pub fn refresh_queue_snapshot(&mut self) {
        if let Some(queues) = self.steer_queues.as_ref() {
            self.queued = queues.snapshot();
        }
    }

    /// Stop the running agent, keeping the session usable.
    ///
    /// The partial answer is deliberately not discarded here: the engine hands it
    /// back when the run settles and the transcript is rebuilt from that, so
    /// clearing it would be the one thing the user cannot undo. The status stays
    /// `Stopping` until then, because a second run started while the first is
    /// still unwinding would race it for the same conversation.
    ///
    /// Returns `false` when there was nothing to stop.
    pub fn stop_run(&mut self) -> bool {
        let Some(token) = self.cancel_token.take() else {
            return false;
        };
        // Hand queued input back: dropping it silently is the other thing a user
        // cannot recover from.
        let restored = self.restore_queue_to_input();
        token.cancel();
        self.agent_status = AgentStatus::Stopping;
        self.set_status_message(if restored > 0 {
            format!("Stopping… {restored} queued message(s) back in the editor.")
        } else {
            "Stopping…".to_string()
        });
        true
    }

    /// Whether the transcript already carries a result for this tool call.
    ///
    /// Lets a result that arrives *after* the run reported itself finished still
    /// land, without a second copy landing with it.
    fn has_tool_result(&self, tool_call_id: &str) -> bool {
        self.conversation.messages.iter().any(|message| {
            matches!(message, ChatMessage::Tool { tool_call_id: id, .. } if id == tool_call_id)
        })
    }

    // ── Run control: pause / resume ───────────────────────────────────────

    /// Whether a run is currently executing (i.e. pause is meaningful).
    pub fn is_running(&self) -> bool {
        matches!(
            self.agent_status,
            AgentStatus::Thinking
                | AgentStatus::Streaming
                | AgentStatus::ExecutingTool { .. }
                | AgentStatus::Stopping
        )
    }
    /// The glyph for the current spinner frame.
    pub fn spinner(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()]
    }

    /// One animation step. Driven by terminal ticks, so the spinner moves only
    /// as fast as frames are drawn.
    pub fn tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    pub fn is_paused(&self) -> bool {
        self.pause_gate.as_ref().is_some_and(|g| g.is_paused())
    }

    /// Cooperatively pause the running agent; takes effect at the next tool
    /// boundary, never mid-tool.
    pub fn pause_run(&mut self) {
        match &self.pause_gate {
            Some(gate) if self.is_running() => {
                gate.pause();
                self.set_status_message(
                    "⏸ Paused — holds at the next tool boundary (/unpause to resume)",
                );
            }
            Some(_) => self.set_status_message("Nothing to pause: no run is executing."),
            None => self.set_status_message("Nothing to pause: no run is executing."),
        }
    }

    pub fn resume_run(&mut self) {
        match &self.pause_gate {
            Some(gate) if gate.is_paused() => {
                gate.resume();
                self.set_status_message("▶ Resumed.");
            }
            Some(_) => self.set_status_message("The run is not paused."),
            None => self.set_status_message("No paused run to resume."),
        }
    }

    // ── Question modal (ask_user_question) ──────────────────────────────

    fn handle_question_key(&mut self, key: KeyEvent) {
        let Some(pending) = self.pending_question.as_mut() else {
            return;
        };
        let has_options = pending.current().is_some_and(|q| !q.options.is_empty());

        match key.code {
            KeyCode::Esc => {
                if let Some(pending) = self.pending_question.take() {
                    pending.resolve(serde_json::Value::Null);
                }
                self.set_status_message(
                    "Question dismissed — the agent proceeds with assumptions.",
                );
            }
            KeyCode::Up if has_options => {
                if let Some(q) = pending.current() {
                    if !q.options.is_empty() {
                        pending.selected = pending.selected.saturating_sub(1);
                    }
                }
            }
            KeyCode::Down if has_options => {
                if let Some(q) = pending.current() {
                    let count = q.options.len();
                    if count > 0 {
                        pending.selected = (pending.selected + 1).min(count - 1);
                    }
                }
            }
            KeyCode::Char(' ') if has_options => {
                let multi = pending.current().is_some_and(|q| q.multi_select);
                if multi {
                    let idx = pending.selected;
                    if pending.toggled.contains(&idx) {
                        pending.toggled.retain(|&i| i != idx);
                    } else {
                        pending.toggled.push(idx);
                    }
                }
            }
            KeyCode::Enter => {
                self.submit_current_question();
            }
            KeyCode::Backspace if !has_options => {
                pending.input.pop();
            }
            KeyCode::Char(c) if !has_options => {
                pending.input.push(c);
            }
            _ => {}
        }
    }

    /// Answer the current question from the modal state and resolve the set
    /// when the last one is answered.
    fn submit_current_question(&mut self) {
        let Some(pending) = self.pending_question.as_mut() else {
            return;
        };
        let Some(q) = pending.current().cloned() else {
            return;
        };

        let answer = if q.options.is_empty() {
            if pending.input.trim().is_empty() {
                return; // Enter on empty free-form input is a no-op
            }
            pending.input.trim().to_string()
        } else if q.multi_select {
            let picks: Vec<usize> = if pending.toggled.is_empty() {
                vec![pending.selected]
            } else {
                let mut t = pending.toggled.clone();
                t.sort_unstable();
                t
            };
            picks
                .into_iter()
                .filter_map(|i| q.options.get(i).map(|o| o.label.clone()))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            q.options
                .get(pending.selected)
                .map(|o| o.label.clone())
                .unwrap_or_default()
        };

        if let Some(payload) = pending.answer_current(answer) {
            if let Some(pending) = self.pending_question.take() {
                pending.resolve(payload);
            }
            self.set_status_message("✔ Answer sent to the agent.");
        }
    }

    // ── Thinking level / permission ─────────────────────────────

    /// Effective thinking level: manual override → conversation binding →
    /// the resolved model's default.
    pub fn effective_thinking_level(&self) -> Option<String> {
        self.thinking_level
            .clone()
            .or_else(|| self.conversation.thinking_level.clone())
            .or_else(|| {
                self.provider_registry
                    .resolve(&self.model.selection_id())
                    .map(|s| s.default_thinking_level.clone())
            })
    }

    fn set_thinking_level(&mut self, level: Option<String>) {
        self.thinking_level = level.clone();
        self.conversation.thinking_level = level;
        self.save_and_refresh();
    }

    // ── Traces ─────────────────────────────────────────────────────────────

    /// Start recording a filtered trace for a new run.
    fn begin_trace(&mut self, prompt: &str) {
        self.trace_events = Some(Vec::new());
        self.trace_meta = Some(TraceRunMeta {
            task_id: format!("run_{}", now_ms()),
            started_at_ms: now_ms(),
            prompt: prompt.to_string(),
            model: self.model.selection_id(),
        });
    }

    /// Tiered retention, mirroring the daemon: streaming micro-deltas never
    /// reach the in-memory trace (they would blow up long tasks); everything
    /// else is kept.
    fn record_trace_event(&mut self, event: ObservedEvent) {
        let is_micro_delta = matches!(
            event.event,
            AgentEvent::TokenDelta { .. }
                | AgentEvent::ReasoningDelta { .. }
                | AgentEvent::ToolCallChunk { .. }
        );
        if !is_micro_delta {
            if let Some(events) = self.trace_events.as_mut() {
                if events.len() < 5000 {
                    events.push(event);
                }
            }
        }
    }

    /// Persist the completed run's trace next to the session (same layout as
    /// the daemon: `<store>/<session>/traces/<task_id>.json`).
    fn finalize_trace(
        &self,
        success: bool,
        final_text: Option<&str>,
        run_stats: Option<&AgentStats>,
    ) {
        let (Some(events), Some(meta)) = (self.trace_events.as_ref(), self.trace_meta.as_ref())
        else {
            return;
        };
        let Some(store) = self.store.as_ref() else {
            return;
        };

        let finished_at_ms = now_ms();
        let trace_data = serde_json::json!({
            "task_id": meta.task_id,
            "session_id": self.conversation.id,
            "model": meta.model,
            "workspace_dir": self.workspace_dir.display().to_string(),
            "prompt": meta.prompt,
            "started_at_ms": meta.started_at_ms,
            "finished_at_ms": finished_at_ms,
            "duration_ms": run_stats.map(|s| s.total_duration_ms).unwrap_or(0),
            "wall_duration_ms": finished_at_ms.saturating_sub(meta.started_at_ms),
            "finish_reason": if success { "Done" } else { "Error" },
            "stats": run_stats,
            "final_content": final_text,
            "events": events,
        });

        let root = store.root().to_path_buf();
        let session_id = self.conversation.id.clone();
        let task_id = meta.task_id.clone();
        tokio::spawn(async move {
            save_task_trace(&root, &session_id, &task_id, &trace_data).await;
        });
    }

    /// Open an interactive picker over the current session's saved traces.
    fn spawn_trace_picker(&mut self, event_tx: mpsc::UnboundedSender<crate::event::AppEvent>) {
        self.set_status_message("Scanning session traces...");
        let Some(store) = self.store.clone() else {
            self.set_status_message("No conversation store configured.");
            return;
        };
        let session_id = self.conversation.id.clone();
        tokio::spawn(async move {
            let traces = list_session_traces(store.root(), &session_id).await;
            let items: Vec<PickerItem> = traces
                .iter()
                .map(|t| {
                    let task_id = t.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let prompt = t
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .map(|p| thunder_agent_providers::naming::truncate_chars(p, 60))
                        .unwrap_or_default();
                    let dur = t.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    PickerItem::new(
                        task_id,
                        task_id.to_string(),
                        format!("{prompt} · {}ms", dur),
                    )
                    .with_badge(
                        t.get("finish_reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?"),
                    )
                })
                .collect();
            let _ = event_tx.send(crate::event::AppEvent::OpenPicker {
                kind: PickerKind::SelectTrace,
                title: "🧾 Session Task Traces (↑/↓ to move, Enter to inspect)".to_string(),
                items,
                empty_message: Some(
                    "No traces recorded for this session yet. Traces are saved automatically after each run.".to_string(),
                ),
            });
        });
    }

    /// Render a saved trace as markdown, delivered into the conversation.
    fn spawn_trace_view(
        &mut self,
        task_id: Option<&str>,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let Some(store) = self.store.clone() else {
            self.set_status_message("No conversation store configured.");
            return;
        };
        let session_id = self.conversation.id.clone();
        let task_id = task_id.map(str::to_string);
        tokio::spawn(async move {
            let out = match load_task_trace(store.root(), &session_id, task_id.as_deref()).await {
                Some(val) => render_trace_markdown(&val),
                None => "❌ No trace found for this session. Run a task first — traces are saved automatically after each run.".to_string(),
            };
            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                agent_id: "trace_viewer".to_string(),
                success: true,
                final_text: Some(out),
                authoritative_messages: None,
                raw_messages: None,
                run_stats: None,
                finish_reason: None,
            });
        });
    }

    /// Render the trace index for the current session as markdown.
    fn spawn_trace_list(&mut self, event_tx: mpsc::UnboundedSender<crate::event::AppEvent>) {
        let Some(store) = self.store.clone() else {
            self.set_status_message("No conversation store configured.");
            return;
        };
        let session_id = self.conversation.id.clone();
        tokio::spawn(async move {
            let traces = list_session_traces(store.root(), &session_id).await;
            let out = if traces.is_empty() {
                "### 🧾 Task Traces\n\nNo traces recorded for this session yet.".to_string()
            } else {
                let mut out = String::from("### 🧾 Task Traces\n\n| Task | Finish | Duration | Prompt |\n|---|---|---|---|\n");
                for t in &traces {
                    let task_id = t.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let reason = t
                        .get("finish_reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let dur = t.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let prompt = t
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .map(|p| thunder_agent_providers::naming::truncate_chars(p, 40))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "| `{task_id}` | {reason} | {dur}ms | {prompt} |\n"
                    ));
                }
                out.push_str("\n*Inspect one with `/trace <task_id>`.*");
                out
            };
            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                agent_id: "trace_viewer".to_string(),
                success: true,
                final_text: Some(out),
                authoritative_messages: None,
                raw_messages: None,
                run_stats: None,
                finish_reason: None,
            });
        });
    }

    // ── Titles ─────────────────────────────────────────────────────────────

    /// Manually set (and lock) the conversation title.
    fn set_manual_title(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.set_status_message("Title text must not be empty.");
            return;
        }
        self.conversation.title = Some(thunder_agent_providers::naming::clamp_title(
            trimmed.to_string(),
        ));
        self.conversation.title_source = Some("manual".to_string());
        self.conversation.updated_at_ms = now_ms();
        self.set_status_message(format!(
            "Title set: {}",
            self.conversation.title.clone().unwrap_or_default()
        ));
        self.save_and_refresh();
    }

    /// Whether a finished run should trigger background auto-titling: the
    /// conversation is still untitled, or this was its first exchange.
    pub fn should_autogenerate_title(&self) -> bool {
        self.conversation.is_title_placeholder() || self.run_is_first_exchange
    }

    /// Kick off background title generation (never blocks the UI loop).
    pub fn spawn_title_generation(
        &mut self,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
        force: bool,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let registry = self.provider_registry.clone();
        let session_id = self.conversation.id.clone();
        self.set_status_message("Generating title...");
        tokio::spawn(async move {
            let result =
                crate::title::generate_conversation_title(&store, &registry, &session_id, force)
                    .await
                    .map_err(|e| e.to_string());
            let _ = event_tx.send(crate::event::AppEvent::TitleGenerated { session_id, result });
        });
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        // 0. The question modal outranks everything: the agent is blocked on it.
        if self.pending_question.is_some() {
            self.handle_question_key(key);
            return;
        }

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
                if !self.stop_run() {
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
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Fast path to every link in the session, for when the mouse is
                // not delivering clicks (or is busy being a terminal gesture).
                self.open_links_picker();
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
                if self.focus == FocusPane::Input {
                    // Inside a command's argument slot, Tab changes the value
                    // in place; only once there is no value to change does it
                    // fall back to completing the command name.
                    if let Some(next) =
                        crate::commands::arg_cycle(&self.input, crate::commands::CycleDir::Forward)
                    {
                        self.set_input(next);
                        return;
                    }
                    // Complete the command name only while it is still being
                    // typed: rewriting a line that already carries free-text
                    // arguments would silently discard them.
                    if self.input.starts_with('/') && !self.input.contains(char::is_whitespace) {
                        let matches = crate::commands::filter_commands(&self.input);
                        if !matches.is_empty() {
                            let selected = matches[self.command_popup_idx % matches.len()];
                            self.set_input(format!("/{} ", selected.name));
                            return;
                        }
                    }
                }
                self.cycle_focus();
            }
            // Shift+Tab walks argument values backwards. It has no other
            // binding in the input pane, so it stays inert elsewhere.
            KeyCode::BackTab => {
                if self.focus == FocusPane::Input {
                    if let Some(next) =
                        crate::commands::arg_cycle(&self.input, crate::commands::CycleDir::Backward)
                    {
                        self.set_input(next);
                    }
                }
            }
            KeyCode::Esc => match self.mode {
                ViewMode::Help | ViewMode::SessionList => {
                    self.mode = ViewMode::Chat;
                    self.focus = FocusPane::Input;
                }
                ViewMode::Chat => {
                    if self.input.starts_with('/') {
                        self.clear_input();
                        self.command_popup_idx = 0;
                    } else {
                        self.stop_run();
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
                        self.set_input(format!("/{} ", selected.name));
                        self.command_popup_idx = 0;
                        return;
                    }
                }

                // A run is already in flight: Enter steers it, Alt+Enter queues a
                // follow-up. Neither interrupts a tool that is executing — both are
                // delivered at the next turn boundary.
                if self.is_running() && !self.input.trim().is_empty() {
                    let behavior = if key.modifiers.contains(KeyModifiers::ALT) {
                        QueueBehavior::FollowUp
                    } else {
                        QueueBehavior::Steer
                    };
                    self.enqueue_for_run(behavior);
                    return;
                }

                if !self.input.trim().is_empty()
                    && (self.agent_status == AgentStatus::Idle
                        || matches!(self.agent_status, AgentStatus::Done | AgentStatus::Error(_)))
                {
                    let prompt = std::mem::take(&mut self.input);
                    self.input_cursor = 0;
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
            // Caret movement. Cmd arrives as `SUPER`; Home/End and Ctrl+A/Ctrl+E
            // are the fallbacks, since Terminal.app never forwards the Command key.
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SUPER) => self.cursor_home(),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => self.cursor_prev_word(),
            KeyCode::Left => self.cursor_left(),
            KeyCode::Right if key.modifiers.contains(KeyModifiers::SUPER) => self.cursor_end(),
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => self.cursor_next_word(),
            KeyCode::Right => self.cursor_right(),
            KeyCode::Home if !self.input.is_empty() => self.cursor_home(),
            KeyCode::End if !self.input.is_empty() => self.cursor_end(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_home()
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor_end()
            }
            // Ctrl+J inserts a literal newline. Enter submits (or steers), so a
            // multi-line prompt needs a key of its own.
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char('\n');
                self.command_popup_idx = 0;
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                self.command_popup_idx = 0;
            }
            KeyCode::Backspace => {
                self.backspace();
                self.command_popup_idx = 0;
            }
            KeyCode::Delete => {
                self.delete_forward();
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
                // Argument cycling wins over the command list and over history:
                // in this slot the arrows mean "change the value", matching Tab.
                if let Some(next) =
                    crate::commands::arg_cycle(&self.input, crate::commands::CycleDir::Backward)
                {
                    self.set_input(next);
                    return;
                }

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
                    self.set_input(self.input_history[next_idx].clone());
                } else if self.input.is_empty() {
                    self.scroll_up(1);
                }
            }
            KeyCode::Down => {
                if let Some(next) =
                    crate::commands::arg_cycle(&self.input, crate::commands::CycleDir::Forward)
                {
                    self.set_input(next);
                    return;
                }

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
                        self.set_input(self.input_history[next_idx].clone());
                    } else {
                        self.history_idx = None;
                        self.clear_input();
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
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // The transcript sits behind any open modal; a click belongs to
                // the modal, so the hitboxes underneath must not fire.
                if self.picker.is_open
                    || self.pending_question.is_some()
                    || self.mode == ViewMode::Help
                {
                    return;
                }
                self.activate_link_at(mouse.column, mouse.row);
            }
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

    /// The clickable target under a screen cell, if any.
    pub fn link_at(&self, column: u16, row: u16) -> Option<LinkTarget> {
        self.link_hitboxes
            .iter()
            .find(|h| h.row == row && column >= h.col_start && column < h.col_end)
            .map(|h| h.target.clone())
    }

    /// Act on a click: reveal local files, open web URLs.
    pub fn activate_link_at(&mut self, column: u16, row: u16) {
        if let Some(target) = self.link_at(column, row) {
            self.launch_link(&target);
            return;
        }
        // A click that lands on a link's row but beside the text is the most
        // likely way to miss, and silence there is indistinguishable from the
        // feature being broken. Say so instead of doing nothing.
        if let Some(near) = self.link_hitboxes.iter().find(|h| h.row == row) {
            self.set_status_message(format!(
                "No link at that column — the one on this line starts at column {}. Or use /links.",
                near.col_start + 1
            ));
        }
    }

    /// Directories a relative link path may resolve in: the workspace first,
    /// then any extra roots.
    pub fn workspace_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::with_capacity(1 + self.extra_roots.len());
        roots.push(self.workspace_dir.clone());
        roots.extend(self.extra_roots.iter().cloned());
        roots
    }

    /// Forget memoised path lookups. Call whenever the roots or the session
    /// change: those are the only inputs that alter the answer.
    pub fn clear_link_cache(&mut self) {
        self.link_cache.clear();
    }

    /// Collect every link and file path mentioned in the session into the
    /// picker, so they can be acted on without a mouse.
    pub fn open_links_picker(&mut self) {
        let roots = self.workspace_roots();
        // The system prompt is boilerplate full of backticked tokens; only what
        // the conversation actually said is worth offering to open.
        let texts: Vec<String> = self
            .conversation
            .messages
            .iter()
            .filter(|m| {
                !matches!(
                    m,
                    thunder_agent_loop::types::message::ChatMessage::System { .. }
                )
            })
            .filter_map(|m| m.content_str().map(str::to_string))
            .collect();

        let mut targets: Vec<LinkTarget> = Vec::new();
        for text in &texts {
            for target in crate::links::extract_links(text, &roots, &mut self.link_cache) {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }

        if targets.is_empty() {
            self.conversation.add_assistant_message(
                Some("No links or file paths found in this session.".to_string()),
                None,
            );
            self.save_and_refresh();
            return;
        }

        // The item id is the index into `pending_links`, which keeps the
        // resolved target intact across the picker round-trip.
        let items = targets
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let action = match t {
                    LinkTarget::Url(_) => "Open in browser",
                    LinkTarget::File(_) => "Reveal in Finder",
                };
                PickerItem::new(idx.to_string(), t.display(), action).with_badge(t.kind_label())
            })
            .collect();

        self.pending_links = targets;
        self.picker.open(
            PickerKind::SelectLink,
            Some(PickerKind::SelectLink.default_title().to_string()),
            items,
        );
    }

    /// Reveal a local file or open a URL, reporting the outcome in the status
    /// bar (a silent no-op would read as a missed click).
    fn launch_link(&mut self, target: &LinkTarget) {
        let outcome = match &self.link_opener {
            Some(opener) => opener(target),
            None => crate::links::launch(target),
        };
        match outcome {
            Ok(()) => self.set_status_message(match target {
                LinkTarget::Url(url) => format!("Opened {url}"),
                LinkTarget::File(path) => format!("Revealed {}", path.display()),
            }),
            Err(err) => self.set_status_message(format!("Could not open: {err}")),
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
            .with_system_prompt(thunder_agent_loop::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT);
        self.streaming_delta.clear();
        self.reasoning_delta.clear();
        self.active_tool_calls.clear();
        self.agent_status = AgentStatus::Idle;
        self.scroll_offset = 0;
        self.last_error = None;
        self.active_skill = None;
        self.clear_link_cache();
        self.metrics.reset_for_session();
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
            PickerKind::SelectTrace => {
                self.conversation
                    .add_user_message(format!("/trace {}", item.id));
                self.spawn_trace_view(Some(&item.id), event_tx);
            }
            PickerKind::SlashCommand => {
                self.set_input(format!("/{} ", item.id));
            }
            PickerKind::SelectLink => {
                // The id is the index into `pending_links`.
                if let Some(target) = item
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| self.pending_links.get(idx).cloned())
                {
                    self.launch_link(&target);
                }
            }
        }
    }

    /// Stage local image files for the next prompt.
    ///
    /// Bytes are validated with the shared ingress rules (magic bytes + size),
    /// so a mislabelled or oversized file is rejected with a status message
    /// rather than being sent to the model.
    fn handle_image_command(&mut self, args: &[&str]) {
        if args.is_empty() {
            self.set_status_message(
                "Usage: /image <path> [path...] — png/jpeg/webp/gif".to_string(),
            );
            return;
        }

        let mut added = 0usize;
        let mut error: Option<String> = None;

        for arg in args {
            if self.pending_images.len() >= MAX_IMAGES_PER_MESSAGE {
                error = Some(format!(
                    "At most {MAX_IMAGES_PER_MESSAGE} images per prompt"
                ));
                break;
            }

            let raw = Path::new(arg);
            let path = if raw.is_relative() {
                self.workspace_dir.join(raw)
            } else {
                raw.to_path_buf()
            };
            let label = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image")
                .to_string();

            match std::fs::read(&path) {
                Ok(bytes) => match validate_image_bytes(&bytes, &label) {
                    Ok((mime, data)) => {
                        self.pending_images.push(ContentPart::Image {
                            mime_type: mime.to_string(),
                            data,
                            name: Some(label),
                            path: None,
                            sha256: None,
                        });
                        added += 1;
                    }
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                },
                Err(e) => {
                    error = Some(format!("Cannot read '{}': {e}", path.display()));
                    break;
                }
            }
        }

        if let Some(err) = error {
            self.set_status_message(format!("🖼 Image error: {err}"));
        } else if added > 0 {
            self.set_status_message(format!(
                "🖼 {} image(s) attached for the next prompt",
                self.pending_images.len()
            ));
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
            // 0. /image <path> [path...] — stage local images for the next prompt
            "image" | "attach" | "img" => {
                self.handle_image_command(args);
                true
            }

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
                                run_stats: None,
                                finish_reason: None,
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
                out.push_str(
                    "- `Ctrl+B`: Toggle Sidebar | `Ctrl+H`: Toggle Help | `Ctrl+C / Esc`: Cancel\n",
                );
                out.push_str("- `/pause` / `/unpause`: Hold & release a run at tool boundaries | `Esc`: Dismiss a question modal\n");

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
                                            run_stats: None,
                                            finish_reason: None,
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
                                        run_stats: None,
                                        finish_reason: None,
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
                                        run_stats: None,
                                        finish_reason: None,
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
                                run_stats: None,
                                finish_reason: None,
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
                        "| `permission` | `{}` | Tool capability tier |\n",
                        self.permission.describe()
                    ));
                    out.push_str(&format!(
                        "| `thinking` | `{}` | Reasoning effort |\n",
                        self.effective_thinking_level()
                            .as_deref()
                            .unwrap_or("model default")
                    ));
                    out.push_str(&format!(
                        "| `roots` | `{}` | Extra workspace roots |\n",
                        self.extra_roots.len()
                    ));
                    out.push_str(&format!(
                        "| `ask_user` | `{}` | ask_user_question mounted |\n",
                        if self.ask_user_enabled { "on" } else { "off" }
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
                        self.clear_link_cache();
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

            // 11b. /preview [on|off|md|rich] — rendered Markdown vs raw source
            "preview" | "md" | "rich" => {
                self.conversation.add_user_message(raw_cmd);
                let requested = args.first().map(|a| a.to_ascii_lowercase());
                self.markdown_enabled = match requested.as_deref() {
                    Some("on" | "true" | "yes") => true,
                    Some("off" | "false" | "no" | "raw") => false,
                    // Bare `/preview` toggles.
                    _ => !self.markdown_enabled,
                };
                let state = if self.markdown_enabled {
                    "✔ Markdown preview on — renders headings, lists, code blocks and clickable links."
                } else {
                    "✔ Markdown preview off — showing raw source (links are not clickable)."
                };
                self.conversation
                    .add_assistant_message(Some(state.to_string()), None);
                self.set_status_message(state.to_string());
                self.save_and_refresh();
                true
            }

            // 11c. /links — every link and file path mentioned in this session
            "links" | "urls" | "files" => {
                self.conversation.add_user_message(raw_cmd);
                self.open_links_picker();
                true
            }

            // 11d. /queue [clear | all | one] — input queued into a live run
            "queue" | "queued" | "pending" => {
                self.conversation.add_user_message(raw_cmd);
                self.refresh_queue_snapshot();
                let out = match args.first().copied() {
                    Some("clear" | "reset") => {
                        let restored = self.restore_queue_to_input();
                        if restored == 0 {
                            "No queued messages.".to_string()
                        } else {
                            format!(
                                "✔ Cleared {restored} queued message(s) and returned them to the editor."
                            )
                        }
                    }
                    Some(raw) => match QueueMode::parse(raw) {
                        Some(mode) => {
                            if let Some(queues) = self.steer_queues.as_ref() {
                                queues.steering.set_mode(mode);
                                queues.follow_up.set_mode(mode);
                            }
                            format!("✔ Queue mode set to `{}` for this run.", mode.label())
                        }
                        None => {
                            format!("❌ Unknown queue option `{raw}`. Use `clear`, `all` or `one`.")
                        }
                    },
                    None => {
                        let snap = self.queued.clone();
                        if snap.is_empty() {
                            "### 📥 Queued Messages\n\nNothing queued.\n\n*While a run is live: `Enter` steers it, `Alt+Enter` queues a follow-up, `/queue clear` takes them back.*".to_string()
                        } else {
                            let mut out = String::from("### 📥 Queued Messages\n\n");
                            if !snap.steering.is_empty() {
                                out.push_str("**Steering** — enters at the next turn boundary:\n");
                                for (i, m) in snap.steering.iter().enumerate() {
                                    out.push_str(&format!("{}. {}\n", i + 1, m));
                                }
                            }
                            if !snap.follow_up.is_empty() {
                                if !snap.steering.is_empty() {
                                    out.push('\n');
                                }
                                out.push_str("**Follow-up** — enters after the run finishes:\n");
                                for (i, m) in snap.follow_up.iter().enumerate() {
                                    out.push_str(&format!("{}. {}\n", i + 1, m));
                                }
                            }
                            out.push_str("\n*`/queue clear` returns them to the editor.*");
                            out
                        }
                    }
                };
                self.conversation.add_assistant_message(Some(out), None);
                self.save_and_refresh();
                true
            }

            // 12. /health or /doctor — lightweight local diagnostics (no LLM call)
            // 11e. /metrics [on|off] — the token / cache / speed bar
            "metrics" | "meters" => {
                self.conversation.add_user_message(raw_cmd);
                self.metrics.enabled = match args.first().map(|a| a.to_ascii_lowercase()).as_deref()
                {
                    Some("on" | "true" | "yes" | "show") => true,
                    Some("off" | "false" | "no" | "hide") => false,
                    // Bare `/metrics` toggles.
                    _ => !self.metrics.enabled,
                };
                let out = if self.metrics.enabled {
                    "✔ Metrics bar on."
                } else {
                    "✔ Metrics bar off — the row goes back to the transcript."
                };
                self.conversation
                    .add_assistant_message(Some(out.to_string()), None);
                self.set_status_message(out.to_string());
                self.save_and_refresh();
                true
            }

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

            // /pause — cooperatively hold the running agent at the next tool boundary
            "pause" | "hold" => {
                self.conversation.add_user_message(raw_cmd);
                self.pause_run();
                true
            }

            // /unpause — release a paused run
            "unpause" | "go" | "continue" => {
                self.conversation.add_user_message(raw_cmd);
                self.resume_run();
                true
            }

            // /think [off|low|medium|high] — reasoning effort for the next run
            "think" | "thinking" => {
                self.conversation.add_user_message(raw_cmd);
                match args.first().map(|a| a.to_lowercase()).as_deref() {
                    Some("off") | Some("none") => {
                        self.set_thinking_level(Some("off".to_string()));
                        self.conversation.add_assistant_message(
                            Some(
                                "✔ Thinking level: **off** (bound to this conversation)"
                                    .to_string(),
                            ),
                            None,
                        );
                    }
                    Some("low") | Some("medium") | Some("high") => {
                        let lvl = args.first().unwrap().to_lowercase();
                        self.set_thinking_level(Some(lvl.clone()));
                        self.conversation.add_assistant_message(
                            Some(format!(
                                "✔ Thinking level: **{lvl}** (bound to this conversation)"
                            )),
                            None,
                        );
                    }
                    Some(other) => {
                        self.conversation.add_assistant_message(
                            Some(format!(
                                "❌ Unknown thinking level `{other}`. Available: `off`, `low`, `medium`, `high`"
                            )),
                            None,
                        );
                    }
                    None => {
                        let effective = self.effective_thinking_level();
                        let out = format!(
                            "### 🧠 Thinking Level\n\n- Explicit override: `{}`\n- Conversation binding: `{}`\n- Effective for the next run: **`{}`**\n\n*Set with `/think <off|low|medium|high>`*",
                            self.thinking_level.as_deref().unwrap_or("—"),
                            self.conversation.thinking_level.as_deref().unwrap_or("—"),
                            effective.as_deref().unwrap_or("model default"),
                        );
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                }
                true
            }

            // /permission [read|write|bash] — manual capability tier
            "permission" | "perm" => {
                self.conversation.add_user_message(raw_cmd);
                let tier = match args.first().map(|a| a.to_lowercase()).as_deref() {
                    Some("read" | "readonly" | "ro") => Some(Permission::Read),
                    Some("write" | "readwrite" | "rw") => Some(Permission::Write),
                    Some("bash" | "full") => Some(Permission::Bash),
                    Some(other) => {
                        self.conversation.add_assistant_message(
                            Some(format!(
                                "❌ Unknown permission tier `{other}`. Available: `read`, `write`, `bash`"
                            )),
                            None,
                        );
                        None
                    }
                    None => {
                        let out = format!(
                            "### 🔐 Permission Tier\n\n- Current: **{}**\n\n*Set with `/permission <read|write|bash>`*",
                            self.permission.describe()
                        );
                        self.conversation.add_assistant_message(Some(out), None);
                        None
                    }
                };
                if let Some(tier) = tier {
                    self.permission = tier;
                    self.conversation.add_assistant_message(
                        Some(format!("✔ Permission tier set to **{}**", tier.describe())),
                        None,
                    );
                    self.set_status_message(format!("Permission: {}", tier.describe()));
                }
                self.save_and_refresh();
                true
            }

            // /roots [add <path> | remove <path|#> | clear] — extra workspace roots
            "roots" | "root" | "shared" => {
                self.conversation.add_user_message(raw_cmd);
                match args.first().copied() {
                    Some("add" | "grant") => {
                        let rest = args[1..].join(" ");
                        let pb = PathBuf::from(&rest);
                        if pb.is_dir() {
                            if !self.extra_roots.contains(&pb) {
                                self.extra_roots.push(pb.clone());
                                self.clear_link_cache();
                            }
                            self.conversation.add_assistant_message(
                                Some(format!(
                                    "✔ Extra root granted: `{}` ({} total)\n\nRead/write standing is shared with the primary workspace on the next run.",
                                    pb.display(),
                                    self.extra_roots.len()
                                )),
                                None,
                            );
                        } else {
                            self.conversation.add_assistant_message(
                                Some(format!("❌ Directory `{}` does not exist.", pb.display())),
                                None,
                            );
                        }
                    }
                    Some("remove" | "revoke") => {
                        let target = args[1..].join(" ");
                        let before = self.extra_roots.len();
                        if let Ok(idx) = target.parse::<usize>() {
                            if idx < self.extra_roots.len() {
                                self.extra_roots.remove(idx);
                            }
                        } else {
                            self.extra_roots
                                .retain(|r| r.display().to_string() != target);
                        }
                        self.clear_link_cache();
                        let out = if self.extra_roots.len() < before {
                            format!("✔ Root removed ({} remaining).", self.extra_roots.len())
                        } else {
                            "No matching extra root found.".to_string()
                        };
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                    Some("clear" | "reset") => {
                        let count = self.extra_roots.len();
                        self.extra_roots.clear();
                        self.clear_link_cache();
                        self.conversation.add_assistant_message(
                            Some(format!("✔ Cleared {count} extra root(s).")),
                            None,
                        );
                    }
                    _ => {
                        let mut out = String::from("### 📂 Extra Workspace Roots\n\n");
                        if self.extra_roots.is_empty() {
                            out.push_str("None. Only the primary workspace is writable.\n\n");
                        } else {
                            for (idx, root) in self.extra_roots.iter().enumerate() {
                                out.push_str(&format!("{idx}. `{}`\n", root.display()));
                            }
                            out.push('\n');
                        }
                        out.push_str("*Manage with `/roots add <path>`, `/roots remove <path|#>`, `/roots clear`.*");
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                }
                self.save_and_refresh();
                true
            }

            // /trace [list | <task_id>] — execution traces
            "trace" | "traces" => {
                self.conversation.add_user_message(raw_cmd);
                match args.first().copied() {
                    Some("list") => {
                        self.spawn_trace_list(event_tx.clone());
                    }
                    Some(task_id) => {
                        self.spawn_trace_view(Some(task_id), event_tx.clone());
                    }
                    None => {
                        self.spawn_trace_picker(event_tx.clone());
                    }
                }
                true
            }

            // /title [text | force] — set manually or (re)generate via utility model
            "title" | "rename" => {
                match args.first().copied() {
                    Some("force") | Some("regen") | Some("auto") => {
                        self.conversation.add_user_message(raw_cmd);
                        self.spawn_title_generation(event_tx.clone(), true);
                    }
                    _ if !args.is_empty() => {
                        let text = args.join(" ");
                        self.conversation.add_user_message(raw_cmd);
                        self.set_manual_title(&text);
                    }
                    _ => {
                        self.conversation.add_user_message(raw_cmd);
                        if self.conversation.is_title_manual() {
                            self.conversation.add_assistant_message(
                                Some(
                                    "This conversation's title was set manually; use `/title force` to regenerate it."
                                        .to_string(),
                                ),
                                None,
                            );
                            self.save_and_refresh();
                        } else {
                            self.spawn_title_generation(event_tx.clone(), false);
                        }
                    }
                }
                true
            }

            // /ask [on|off] — mount/unmount the ask_user_question tool
            "ask" | "ask_user" => {
                self.conversation.add_user_message(raw_cmd);
                match args.first().map(|a| a.to_lowercase()).as_deref() {
                    Some("on" | "true" | "yes") => {
                        self.ask_user_enabled = true;
                        self.conversation.add_assistant_message(
                            Some("✔ `ask_user_question` mounted: the agent can ask clarifying questions (answered in a terminal modal).".to_string()),
                            None,
                        );
                    }
                    Some("off" | "false" | "no") => {
                        self.ask_user_enabled = false;
                        self.conversation.add_assistant_message(
                            Some("✔ `ask_user_question` unmounted.".to_string()),
                            None,
                        );
                    }
                    _ => {
                        let out = format!(
                            "ask_user_question: **{}**\n\n*Toggle with `/ask on` / `/ask off`.*",
                            if self.ask_user_enabled { "on" } else { "off" }
                        );
                        self.conversation.add_assistant_message(Some(out), None);
                    }
                }
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

        // Fold any staged images into the user turn. Text stays the trigger /
        // title projection; the images ride along as multimodal parts.
        if self.pending_images.is_empty() {
            self.conversation.add_user_message(prompt.clone());
        } else {
            let images = std::mem::take(&mut self.pending_images);
            self.conversation
                .add_user_message_with_parts(prompt.clone(), images);
        }
        self.agent_status = AgentStatus::Thinking;
        self.streaming_delta.clear();
        self.reasoning_delta.clear();
        self.active_tool_calls.clear();
        self.last_error = None;
        self.auto_scroll = true;

        // Bind run-wide settings to the conversation (mirrors the daemon):
        // model / workspace / thinking level / shared roots persist across runs.
        self.conversation.model = Some(self.model.selection_id());
        self.conversation.workspace = Some(self.workspace_dir.display().to_string());
        if let Some(tl) = self.effective_thinking_level() {
            self.conversation.thinking_level = Some(tl);
        }
        self.conversation.shared_roots = self
            .extra_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        // Auto-title trigger: this run is the conversation's first exchange.
        self.run_is_first_exchange = self
            .conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::User { .. }))
            .count()
            <= 1;

        self.begin_trace(&prompt);

        self.save_and_refresh();

        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());
        let pause_gate = PauseGate::new_shared();
        self.pause_gate = Some(pause_gate.clone());
        // Steering / follow-up input for this run. The host keeps the Arc so it
        // can accept input while the run holds the handle.
        let steer_queues = SteerQueues::new_shared();
        self.steer_queues = Some(Arc::clone(&steer_queues));
        self.queued = QueueSnapshot::default();
        self.queued_turns.clear();
        // The live counters restart here; the previous run's numbers stay on the
        // bar until this one replaces them.
        self.metrics.begin_run();

        match mode {
            ExecutionMode::SingleAgent => {
                self.run_single_agent(prompt, cancel, pause_gate, steer_queues, event_tx);
            }
            ExecutionMode::AutoRouter => {
                self.run_root_agent(prompt, cancel, pause_gate, steer_queues, event_tx);
            }
        }
    }

    fn run_root_agent(
        &self,
        _prompt: String,
        cancel: CancellationToken,
        pause_gate: Arc<PauseGate>,
        steer_queues: Arc<SteerQueues>,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let model = self.model.selection_id();
        let timeout_ms = self.request_timeout_ms;
        let mut base_cfg = AgentConfig::new(model.clone()).with_unlimited_turns();
        base_cfg.temperature = Some(self.temperature);
        base_cfg.request_timeout_ms = timeout_ms;
        // Prompt-cache routing affinity: bind the run to the conversation id.
        base_cfg.session_id = Some(self.conversation.id.clone());
        if let Some(tl) = self.effective_thinking_level() {
            base_cfg.thinking_level = Some(tl);
        }
        if let Some(spec) = self.provider_registry.resolve(&model) {
            base_cfg.pruning.max_context_tokens = spec.context_window;
            base_cfg.prompt_cache_warm =
                spec.prompt_cache_warm_settings(self.effective_thinking_level().as_deref());
        }

        let mut skills_plugin = SkillsPlugin::default();
        if let Some(handle) = &self.active_skill {
            skills_plugin = skills_plugin.with_skill(thunder_agent_skills::Skill::new(
                handle.name.clone(),
                handle.description.clone(),
                handle.system_prompt_fragment(),
            ));
        }

        // Assemble through the shared host builder so the TUI cannot drift from
        // the daemon's baseline capability set again. The terminal host keeps a
        // memory-backed conversation store (the app owns FS persistence) and
        // opts out of the Node-backed TypeScript plugin host.
        let root = StandardHostBuilder::new(Arc::new(MemoryConversationStore::new()))
            .with_skills(skills_plugin)
            .build(
                ThunderRoot::new(base_cfg.clone())
                    .with_workspace(self.workspace_dir.clone())
                    .with_extra_roots(self.extra_roots.clone())
                    .with_provider_registry(self.provider_registry.clone()),
            );

        // The ask-user capability is on by default; `/ask off` unmounts it.
        let ask_enabled = self.ask_user_enabled;
        let root = if ask_enabled {
            root.with_plugin(AskUserPlugin::new(TuiAskUserTool::new(event_tx.clone())))
        } else {
            root
        };

        let session_id = self.conversation.id.clone();
        let context_input = self.conversation.as_context_input();
        let factory_client = self.client_factory.as_ref().and_then(|f| f(&base_cfg));
        let workspace_dir = self.workspace_dir.clone();
        let permission = self.permission;
        // Captured before the move: a plugin call must be attributable to this
        // run, and the sidecar refuses calls that carry no route.
        let run_route = format!("tui_{}", session_id);

        tokio::spawn(async move {
            // Smart baseline set instead of blanket forcing (mirrors the daemon):
            // - conversation: one-line prompt cost, keeps session semantics alive
            // - skills: catalog is compact one-liners; keeps `load_skill` reachable
            // - mcp: only when this workspace actually configures MCP servers
            let workspace_has_mcp_config = workspace_dir.join("mcp_servers.json").exists()
                || workspace_dir.join(".mcp.json").exists();

            let options = RootRunOptions {
                session_id: Some(session_id),
                custom_client: factory_client,
                cancellation_token: Some(cancel),
                // The TUI does not register the script host, so TS plugins are never forced
                // here even when files exist; the daemon is the interactive path.
                forced_plugins: Some(baseline_forced_plugins(workspace_has_mcp_config, false)),
                register_builtins: true,
                thinking_level: None,
                permission,
                pause_gate: Some(pause_gate),
                steer_queues: Some(steer_queues),
                // Forward terminal-native HostUi so approval dialogs (mode: ask / manual)
                // and plugin UI requests present interactive modals rather than failing closed.
                ui: Some(Arc::new(crate::ask_user::TuiHostUi::new(event_tx.clone()))),
                // The TUI is single-run, but a route is still required: the
                // plugin sidecar refuses calls that cannot be attributed.
                route: Some(run_route),
                policy: None,
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
                            let finish_reason_dbg = format!("{:?}", res.run_result.finish_reason);
                            let run_stats = res.run_result.stats.clone();
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
                                run_stats: Some(run_stats),
                                finish_reason: Some(finish_reason_dbg),
                            });
                        }
                        Err(err) => {
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: "root_agent".to_string(),
                                success: false,
                                final_text: Some(err.to_string()),
                                authoritative_messages: None,
                                raw_messages: None,
                                run_stats: None,
                                finish_reason: None,
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
                        run_stats: None,
                        finish_reason: None,
                    });
                }
            }
        });
    }

    fn run_single_agent(
        &self,
        _prompt: String,
        cancel: CancellationToken,
        pause_gate: Arc<PauseGate>,
        steer_queues: Arc<SteerQueues>,
        event_tx: mpsc::UnboundedSender<crate::event::AppEvent>,
    ) {
        let model = self.model.selection_id();
        let timeout_ms = self.request_timeout_ms;
        let mut config = AgentConfig::new(model.clone()).with_unlimited_turns();
        config.temperature = Some(self.temperature);
        config.request_timeout_ms = timeout_ms;
        // Prompt-cache routing affinity: bind the run to the conversation id.
        config.session_id = Some(self.conversation.id.clone());
        if let Some(tl) = self.effective_thinking_level() {
            config.thinking_level = Some(tl);
        }
        // Prompt-cache warming: enabled only when the model declares both a
        // promptCache lifetime and cost pricing in models.json.
        if let Some(spec) = self.provider_registry.resolve(&model) {
            config.prompt_cache_warm =
                spec.prompt_cache_warm_settings(self.effective_thinking_level().as_deref());
        }
        // Capability tier + multi-root jail mirror the host path.
        config.permission = self.permission;
        config.extra_workspace_roots = self.extra_roots.clone();
        if let Some(handle) = &self.active_skill {
            let fragment = handle.system_prompt_fragment();
            config.system_prompt = Some(match config.system_prompt.take() {
                Some(existing) => format!("{existing}\n\n{fragment}"),
                None => fragment,
            });
        }

        let context_input = self.conversation.as_context_input();
        let factory_client = self.client_factory.as_ref().and_then(|f| f(&config));
        let perm = self.permission;
        let ws = self.workspace_dir.clone();

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
                            run_stats: None,
                            finish_reason: None,
                        });
                        return;
                    }
                },
            };
            if let Some(client) = client {
                agent = agent.with_custom_client(client);
            }
            // Gate by capability tier, exactly like the host path: a denied
            // tool is never registered, so it never reaches the model.
            if perm.allows_read() {
                agent.register_tool(Arc::new(
                    ReadFileTool::default().with_default_cwd(ws.clone()),
                ));
                agent.register_tool(Arc::new(GrepTool::default().with_default_cwd(ws.clone())));
                agent.register_tool(Arc::new(FindTool::default().with_default_cwd(ws.clone())));
                agent.register_tool(Arc::new(
                    ListDirTool::default().with_default_cwd(ws.clone()),
                ));
            }
            if perm.allows_write() {
                agent.register_tool(Arc::new(
                    WriteFileTool::default().with_default_cwd(ws.clone()),
                ));
            }
            if perm.allows_exec() {
                agent.register_tool(Arc::new(BashTool::default().with_default_cwd(ws)));
            }
            agent = agent.with_pause_gate(pause_gate);
            agent = agent.with_steer_queues(steer_queues);

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
                            let finish_reason_dbg = format!("{:?}", result.finish_reason);
                            let run_stats = result.stats.clone();
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: result.agent_id,
                                success: is_ok,
                                final_text: result.final_content,
                                authoritative_messages: Some(result.messages),
                                raw_messages: result.raw_messages,
                                run_stats: Some(run_stats),
                                finish_reason: Some(finish_reason_dbg),
                            });
                        }
                        Err(err) => {
                            let _ = event_tx.send(crate::event::AppEvent::AgentFinished {
                                agent_id: "tui_agent".to_string(),
                                success: false,
                                final_text: Some(err.to_string()),
                                authoritative_messages: None,
                                raw_messages: None,
                                run_stats: None,
                                finish_reason: None,
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
                        run_stats: None,
                        finish_reason: None,
                    });
                }
            }
        });
    }

    pub fn handle_agent_event(&mut self, event: ObservedEvent) {
        self.record_trace_event(event.clone());
        match event.event {
            AgentEvent::TurnStart { turn, .. } => {
                self.agent_status = AgentStatus::Thinking;
                info!("[{}] Turn {} started", event.agent_id, turn);
            }
            AgentEvent::TokenDelta { delta, .. } => {
                self.agent_status = AgentStatus::Streaming;
                self.metrics.count_token_delta();
                self.streaming_delta.push_str(&delta);
            }
            AgentEvent::SteerAccepted {
                behavior,
                message,
                image_count,
                ..
            } => {
                // The queued input has joined the transcript. Show it now rather
                // than waiting for the run to end, and drop it from the pending
                // count. A later `authoritative_messages` replaces the whole
                // list, so this cannot double up.
                //
                // The event carries text only, so the turn we actually queued is
                // the one to replay — that is where the attachments are.
                let queued_turn = self
                    .queued_turns
                    .iter()
                    .position(|m| m.content_str() == Some(message.as_str()))
                    .map(|idx| self.queued_turns.remove(idx));
                match queued_turn {
                    Some(ChatMessage::User { content, parts, .. }) => match parts {
                        Some(parts) => self
                            .conversation
                            .add_user_message_with_parts(content, parts),
                        None => self.conversation.add_user_message(content),
                    },
                    _ => self.conversation.add_user_message(message.clone()),
                }
                self.refresh_queue_snapshot();
                let label = if behavior == "follow_up" {
                    "Follow-up"
                } else {
                    "Steering"
                };
                let with_images = if image_count > 0 {
                    format!(" + {image_count} image(s)")
                } else {
                    String::new()
                };
                self.set_status_message(format!(
                    "{label}{with_images} accepted ({} still pending)",
                    self.queued.total()
                ));
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                self.metrics.count_reasoning_delta();
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

                // A cancelled run has its unanswered calls answered by the engine
                // while it unwinds, and those events can arrive after the run has
                // already reported itself finished. Land the result either way —
                // the transcript needs it — but do not resurrect a status for a
                // run that is over.
                if !self.has_tool_result(&tool_call_id) {
                    self.conversation
                        .add_tool_message(&tool_call_id, result.output, Some(name));
                }
                self.active_tool_calls.retain(|c| c.id != tool_call_id);
                if self.cancel_token.is_some() {
                    self.agent_status = AgentStatus::Thinking;
                }
            }
            AgentEvent::TurnEnd { stats, .. } => {
                self.metrics.record_turn(&stats);
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
        run_stats: Option<AgentStats>,
        finish_reason: Option<String>,
    ) {
        // Preserve the raw pre-compaction transcript (if a checkpoint compaction
        // fired) so the working history can be a checkpoint projection while the
        // original history stays auditable on disk.
        self.pending_raw_transcript = raw_messages;

        // A deliberate stop is an outcome, not a failure: no error banner, and the
        // answer the user already read stays put. The engine has closed out every
        // tool call it never ran, so its transcript is valid for the next turn and
        // is the one to keep — the host's own event-driven view may be missing
        // results that arrived after the run reported itself finished.
        let cancelled = finish_reason.as_deref() == Some("Cancelled");
        if cancelled {
            self.agent_status = AgentStatus::Idle;
            match authoritative_messages {
                Some(messages) if !messages.is_empty() => {
                    self.conversation.messages = messages;
                    self.conversation.recalculate_stats();
                }
                _ => {
                    let partial = std::mem::take(&mut self.streaming_delta);
                    if !partial.trim().is_empty() {
                        self.conversation.add_assistant_message(Some(partial), None);
                    }
                }
            }
            self.last_error = None;
            self.set_status_message("Stopped. Ready for your next message.");
        } else if success {
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
            } else if let Some(final_content) = final_text.clone() {
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
                    final_text.clone()
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
        self.pause_gate = None;
        self.steer_queues = None;
        self.queued = QueueSnapshot::default();
        self.queued_turns.clear();
        self.metrics.finish_run(run_stats.as_ref());

        // Lifetime usage bookkeeping (additive, mirroring the daemon):
        // `recalculate_stats` recomputes the working-context estimate, so the
        // running totals must be accumulated separately or every save resets them.
        if let Some(stats) = run_stats.as_ref() {
            let task_tokens = stats.total_prompt_tokens + stats.total_completion_tokens;
            self.conversation.stats.total_used_tokens = self
                .conversation
                .stats
                .total_used_tokens
                .saturating_add(task_tokens);
            self.conversation.stats.turn_count += stats.total_turns;
            self.conversation.stats.tool_calls_count += stats.total_tool_executions;
            self.conversation.stats.duration_ms += stats.total_duration_ms;
        }

        // Persist the run's execution trace (skipped for local UI commands,
        // which carry no stats).
        if self.trace_meta.is_some() {
            let trace_ok = success
                && finish_reason
                    .as_deref()
                    .map(|r| r != "Error" && r != "Cancelled")
                    .unwrap_or(true);
            let trace_text = final_text.clone();
            self.finalize_trace(trace_ok, trace_text.as_deref(), run_stats.as_ref());
            self.run_is_first_exchange = false;
        }
        self.trace_events = None;
        self.trace_meta = None;
    }
}

/// Persist one task trace: `<store>/<session>/traces/<task_id>.json`
/// (same layout as the daemon, so both hosts can read each other's traces).
async fn save_task_trace(
    store_root: &Path,
    session_id: &str,
    task_id: &str,
    trace_data: &serde_json::Value,
) {
    let trace_dir = store_root.join(session_id).join("traces");
    let _ = tokio::fs::create_dir_all(&trace_dir).await;
    let trace_path = trace_dir.join(format!("{task_id}.json"));
    if let Ok(bytes) = serde_json::to_vec_pretty(trace_data) {
        let _ = tokio::fs::write(&trace_path, bytes).await;
    }
}

/// Load a trace by task id, or the most recent one when `task_id` is `None`.
async fn load_task_trace(
    store_root: &Path,
    session_id: &str,
    task_id: Option<&str>,
) -> Option<serde_json::Value> {
    let trace_dir = store_root.join(session_id).join("traces");
    if !trace_dir.exists() {
        return None;
    }

    let target_path = if let Some(tid) = task_id {
        trace_dir.join(format!("{tid}.json"))
    } else {
        let mut entries = tokio::fs::read_dir(&trace_dir).await.ok()?;
        let mut latest_path = None;
        let mut latest_time = std::time::SystemTime::UNIX_EPOCH;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.extension().map(|ext| ext == "json").unwrap_or(false) {
                if let Ok(meta) = entry.metadata().await {
                    if let Ok(modified) = meta.modified() {
                        if modified > latest_time {
                            latest_time = modified;
                            latest_path = Some(p);
                        }
                    }
                }
            }
        }
        latest_path?
    };

    let content = tokio::fs::read(&target_path).await.ok()?;
    serde_json::from_slice(&content).ok()
}

/// Index the session's traces (newest first), mirroring the daemon.
async fn list_session_traces(store_root: &Path, session_id: &str) -> Vec<serde_json::Value> {
    let trace_dir = store_root.join(session_id).join("traces");
    let mut results = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&trace_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.extension().map(|ext| ext == "json").unwrap_or(false) {
                if let Ok(content) = tokio::fs::read(&p).await {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&content) {
                        results.push(serde_json::json!({
                            "task_id": val.get("task_id"),
                            "started_at_ms": val.get("started_at_ms"),
                            "duration_ms": val.get("duration_ms"),
                            "finish_reason": val.get("finish_reason"),
                            "model": val.get("model"),
                            "prompt": val.get("prompt"),
                        }));
                    }
                }
            }
        }
    }
    results.sort_by(|a, b| {
        let ta = a.get("started_at_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let tb = b.get("started_at_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    results
}

/// One timeline line per retained event, best-effort formatted.
fn format_trace_event(event: &ObservedEvent) -> Option<String> {
    match &event.event {
        AgentEvent::TurnStart { turn, .. } => Some(format!("─ turn {turn} started")),
        AgentEvent::TurnEnd { turn, stats, .. } => Some(format!(
            "─ turn {turn} ended ({} tool calls, {}ms)",
            stats.tool_calls_count, stats.duration_ms
        )),
        AgentEvent::ToolExecStart { name, .. } => Some(format!("  ⚙ {name} …")),
        AgentEvent::ToolExecResult { name, result, .. } => Some(format!(
            "  {} {name} — {}ms{}",
            if result.is_error { "✖" } else { "✔" },
            result.duration_ms,
            if result.truncated { " (truncated)" } else { "" }
        )),
        AgentEvent::FileChange {
            path,
            action,
            bytes,
            ..
        } => Some(format!(
            "  📝 {action} {path}{}",
            bytes.map(|b| format!(" ({b}B)")).unwrap_or_default()
        )),
        AgentEvent::TelemetryNotice { layer, action, .. } => {
            Some(format!("  📡 telemetry {layer}/{action}"))
        }
        AgentEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            ..
        } => Some(format!(
            "  🗃 context compacted: ~{} → ~{} tokens",
            tokens_before, tokens_after
        )),
        AgentEvent::Error { message, .. } => Some(format!("  ✖ error: {message}")),
        AgentEvent::SteerAccepted {
            behavior, message, ..
        } => Some(format!("  📥 {behavior} accepted: {message}")),
        AgentEvent::LoopComplete { .. }
        | AgentEvent::TokenDelta { .. }
        | AgentEvent::ReasoningDelta { .. }
        | AgentEvent::ToolCallChunk { .. }
        | AgentEvent::ToolCallReady { .. } => None,
    }
}

/// Render a saved trace document as readable markdown.
pub fn render_trace_markdown(val: &serde_json::Value) -> String {
    let as_str = |key: &str| {
        val.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    let as_u64 = |key: &str| val.get(key).and_then(|v| v.as_u64()).unwrap_or(0);

    let mut out = String::from("### 🧾 Task Trace\n\n");
    out.push_str(&format!(
        "- **Task**: `{}` · **Session**: `{}`\n",
        as_str("task_id"),
        as_str("session_id")
    ));
    out.push_str(&format!(
        "- **Model**: `{}` · **Finish**: {}\n",
        as_str("model"),
        as_str("finish_reason")
    ));
    out.push_str(&format!(
        "- **Duration**: {}ms loop / {}ms wall\n",
        as_u64("duration_ms"),
        as_u64("wall_duration_ms")
    ));
    if let Some(stats) = val.get("stats") {
        let s = |k: &str| stats.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        out.push_str(&format!(
            "- **Stats**: {} turns · {} tool calls · ~{}+{} tokens\n",
            s("total_turns"),
            s("total_tool_executions"),
            s("total_prompt_tokens"),
            s("total_completion_tokens")
        ));
    }
    let prompt = as_str("prompt");
    if !prompt.is_empty() && prompt != "?" {
        out.push_str(&format!(
            "\n**Prompt**: {}\n",
            thunder_agent_providers::naming::truncate_chars(&prompt, 200)
        ));
    }

    out.push_str("\n**Timeline**:\n```\n");
    if let Some(events) = val.get("events").and_then(|v| v.as_array()) {
        for ev in events {
            if let Ok(observed) = serde_json::from_value::<ObservedEvent>(ev.clone()) {
                if let Some(line) = format_trace_event(&observed) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str("```\n");
    out
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
