use serde::{Deserialize, Serialize};
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::ui::{NotifyLevel, UiRequest, UiSource};

/// Incoming command from host (Electron / CLI) via stdin
#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum DaemonRequest {
    /// Ping / Heartbeat check
    Ping { id: Option<String> },
    /// List all configured and available LLM models
    ListModels { id: Option<String> },
    /// List stored conversations
    ListConversations { id: Option<String> },
    /// Get conversation details by session_id
    GetConversation {
        id: Option<String>,
        session_id: String,
    },
    /// Start an Agent task with stream events
    RunTask {
        id: Option<String>,
        task_id: String,
        prompt: String,
        session_id: Option<String>,
        model: Option<String>,
        use_mock: Option<bool>,
        workspace_dir: Option<String>,
        /// Extra roots (e.g. repositories referenced by the task) granted the
        /// same read/write standing as `workspace_dir`. Merged into the
        /// conversation's shared_roots; older daemons ignore this field.
        #[serde(default)]
        extra_workspace_dirs: Option<Vec<String>>,
        thinking_level: Option<String>,
        /// Role id to activate for this run (e.g. "plan"). Resolved against
        /// `~/.thunder/roles.jsonl` and `<workspace>/.arp/roles.jsonl`.
        role: Option<String>,
        /// Approval mode override for this run: `plan` | `ask` |
        /// `accept_edits` | `manual` | `yolo`.
        ///
        /// Omit to keep the session's current mode (so a `set_permission_mode`
        /// issued mid-session sticks), falling back to the role's own `mode`.
        ///
        /// Narrows only: it can clip the role's capability tier (plan mode) and
        /// decide what prompts, but it can never grant a right the role lacks.
        #[serde(default)]
        mode: Option<String>,
    },
    /// List all roles visible from global + workspace scopes
    ListRoles {
        id: Option<String>,
        workspace_dir: Option<String>,
    },
    /// Cooperatively pause a running task at the next tool boundary
    PauseTask { id: Option<String>, task_id: String },
    /// Resume a paused task
    ResumeTask { id: Option<String>, task_id: String },
    /// Answer a pending `ask_user_question` from the agent
    AnswerQuestion {
        id: Option<String>,
        question_id: String,
        #[serde(default)]
        answers: serde_json::Value,
        #[serde(default)]
        cancelled: bool,
    },
    /// Answer a pending `ui_request` dialog (select / confirm / input / editor).
    ///
    /// The `request_id` is issued by the daemon in `ui_request`; a client can only
    /// echo it back. An unknown or expired id is reported as `delivered: false`
    /// and otherwise ignored, so a late answer can never land on a later dialog.
    AnswerUi {
        id: Option<String>,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirmed: Option<bool>,
        #[serde(default)]
        cancelled: bool,
    },
    /// Switch a session's approval mode without starting a task.
    ///
    /// Takes effect on the very next tool call, including one already in flight
    /// in a running task: the gate reads the mode per call rather than baking it
    /// in at pipeline build time.
    SetPermissionMode {
        id: Option<String>,
        session_id: String,
        mode: String,
    },
    /// Report a session's current mode and its remembered "always allow" rules.
    GetPermissionState {
        id: Option<String>,
        session_id: String,
    },
    /// Cancel a running task by task_id
    CancelTask { id: Option<String>, task_id: String },
    /// Reload TypeScript / JavaScript single-file plugins
    ReloadPlugins {
        id: Option<String>,
        path: Option<String>,
    },
    /// Get full execution trace for a task
    GetTrace {
        id: Option<String>,
        session_id: String,
        task_id: Option<String>,
    },
    /// List available task traces for a session
    ListTraces {
        id: Option<String>,
        session_id: String,
    },
    /// AI-generate a conversation title (synchronous, returns title or error detail)
    GenerateTitle {
        id: Option<String>,
        session_id: String,
        /// Force regeneration even if the title was manually set
        #[serde(default)]
        force: bool,
    },
    /// Manually set a conversation title
    SetConversationTitle {
        id: Option<String>,
        session_id: String,
        title: String,
    },
}

/// Outgoing message to host (Electron) via stdout
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonResponse {
    /// Direct request/response acknowledgment
    Response {
        id: Option<String>,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Fine-grained event stream from the Agent (tokens, tools, turns)
    ObservedEvent {
        task_id: String,
        event: ObservedEvent,
    },
    /// Agent task finished successfully
    TaskCompleted {
        task_id: String,
        session_id: String,
        final_content: Option<String>,
        finish_reason: String,
        active_plugins: Vec<String>,
    },
    /// Agent task execution failed or cancelled
    TaskFailed {
        task_id: String,
        session_id: Option<String>,
        error: String,
    },
    /// The agent is asking the user a question and is blocked until answered.
    /// The panel renders this as a question bubble.
    UserQuestion {
        task_id: String,
        session_id: Option<String>,
        question_id: String,
        questions: Vec<QuestionItem>,
    },
    /// A task is paused and will not advance until resumed.
    TaskPaused {
        task_id: String,
        session_id: Option<String>,
        reason: String,
    },
    /// A dialog the agent (or a plugin) wants answered. Blocks that caller until
    /// `answer_ui` arrives or `timeout_ms` elapses — on expiry the daemon
    /// resolves as cancelled, so a silent panel degrades to "denied", never to
    /// "allowed".
    ///
    /// `source` is security-relevant: `host` marks a host-initiated interaction
    /// (e.g. a permission approval) and must be rendered with reserved chrome
    /// that a `plugin`-sourced request cannot imitate.
    UiRequest {
        request_id: String,
        task_id: Option<String>,
        session_id: Option<String>,
        source: UiSource,
        #[serde(flatten)]
        request: UiRequest,
    },
    /// Fire-and-forget notification. A client with no UI simply drops it.
    UiNotice {
        source: UiSource,
        message: String,
        level: NotifyLevel,
    },
    /// Set or clear a status entry in the panel's status bar.
    UiStatus {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// One question presented to the user, mirroring the panel's bubble options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionItem {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
