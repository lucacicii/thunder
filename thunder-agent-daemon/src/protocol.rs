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
        /// Image attachments for the user turn. Path-based attachments are
        /// preferred (the daemon reads and validates them); inline base64 is
        /// accepted for clipboard paste. Older daemons ignore this field.
        #[serde(default)]
        attachments: Option<Vec<Attachment>>,
        /// Non-interactive runs omit the `ask_user_question` tool entirely.
        /// This is the default for daemon clients that do not subscribe to
        /// `user_question`; interactive hosts may explicitly set it to `false`.
        #[serde(default)]
        headless: bool,
    },
    /// Cooperatively pause a running task at the next tool boundary
    PauseTask { id: Option<String>, task_id: String },
    /// Resume a paused task
    ResumeTask { id: Option<String>, task_id: String },
    /// Queue user input into a task that is already running.
    ///
    /// `behavior` must be stated, because the two placements are far apart in
    /// the run and guessing would silently do the wrong one:
    ///
    /// * `steer` — enters after the current turn's tool calls, before the next
    ///   model request. It can keep alive a run that would otherwise have
    ///   concluded.
    /// * `follow_up` — enters only once the run has nothing else to do.
    ///
    /// Neither interrupts a tool that is executing.
    SteerTask {
        id: Option<String>,
        task_id: String,
        message: String,
        behavior: Option<String>,
        /// Image attachments for the queued turn. Same ingress rules as
        /// `run_task`: paths are jailed against the *run's* workspace, and every
        /// payload is magic-byte validated before it can reach the model.
        #[serde(default)]
        attachments: Option<Vec<Attachment>>,
    },
    /// Drop everything queued into a running task and return its text, so a
    /// client can put it back in its editor when the user aborts.
    ClearQueue { id: Option<String>, task_id: String },
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

/// One image attached to a `run_task` request.
///
/// Exactly one of `path` / `data` is required. `path` is read and validated by
/// the daemon (path jail + magic bytes); `data` is a base64 image payload sent
/// by a host that already holds the bytes (e.g. clipboard paste).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(default, alias = "path")]
    pub path: Option<String>,
    #[serde(default, alias = "data")]
    pub data: Option<String>,
    #[serde(default, alias = "mimeType", alias = "mime_type")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
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
        /// The run's terminal reason (`Cancelled`, `Error`, ...) when a run got far
        /// enough to have one. Absent when the daemon failed before the run
        /// existed, so a client can tell "the agent failed" from "we could not
        /// start it".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
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
    /// The task's pending steering / follow-up queues changed.
    ///
    /// Carries both queues in full, never a delta: a client that missed one
    /// message would otherwise drift out of sync with the agent forever.
    TaskQueueUpdate {
        task_id: String,
        session_id: Option<String>,
        steering: Vec<String>,
        follow_up: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: serde_json::Value) -> Result<DaemonRequest, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// `steer_task` carries images the same way `run_task` does, so a host can
    /// steer with a screenshot instead of describing it.
    #[test]
    fn steer_task_accepts_attachments() {
        let req = parse(serde_json::json!({
            "method": "steer_task",
            "id": "req-1",
            "task_id": "task-1",
            "message": "look at this",
            "behavior": "steer",
            "attachments": [
                { "path": "/tmp/shot.png" },
                { "data": "aGVsbG8=", "mimeType": "image/png", "name": "pasted.png" }
            ]
        }))
        .expect("parses");

        match req {
            DaemonRequest::SteerTask {
                behavior,
                attachments,
                ..
            } => {
                assert_eq!(behavior.as_deref(), Some("steer"));
                let attachments = attachments.expect("attachments");
                assert_eq!(attachments.len(), 2);
                assert_eq!(attachments[0].path.as_deref(), Some("/tmp/shot.png"));
                assert_eq!(attachments[1].data.as_deref(), Some("aGVsbG8="));
                assert_eq!(attachments[1].mime_type.as_deref(), Some("image/png"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// Older clients do not send `headless`; they must keep the interactive
    /// question tool. A client that opts in gets the daemon's non-interactive
    /// behavior.
    #[test]
    fn run_task_headless_is_opt_in() {
        let default_req = parse(serde_json::json!({
            "method": "run_task",
            "task_id": "task-1",
            "prompt": "hello"
        }))
        .expect("parses");
        assert!(matches!(
            default_req,
            DaemonRequest::RunTask {
                headless: false,
                ..
            }
        ));

        let headless_req = parse(serde_json::json!({
            "method": "run_task",
            "task_id": "task-2",
            "prompt": "hello",
            "headless": true
        }))
        .expect("parses");
        assert!(matches!(
            headless_req,
            DaemonRequest::RunTask { headless: true, .. }
        ));
    }

    /// Text-only steer stays valid: attachments are optional, and a host that
    /// has none must not be forced to say so.
    #[test]
    fn steer_task_without_attachments_still_parses() {
        let req = parse(serde_json::json!({
            "method": "steer_task",
            "task_id": "task-1",
            "message": "change direction",
            "behavior": "follow_up"
        }))
        .expect("parses");

        match req {
            DaemonRequest::SteerTask {
                behavior,
                attachments,
                ..
            } => {
                assert_eq!(behavior.as_deref(), Some("follow_up"));
                assert!(attachments.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// Omitting `behavior` parses (so the daemon can answer with a useful
    /// error) rather than failing as an unknown request.
    #[test]
    fn steer_task_without_behavior_parses_so_the_daemon_can_refuse_it() {
        let req = parse(serde_json::json!({
            "method": "steer_task",
            "task_id": "task-1",
            "message": "which queue?"
        }))
        .expect("parses");

        assert!(matches!(
            req,
            DaemonRequest::SteerTask { behavior: None, .. }
        ));
    }

    /// A failure that happened before a run existed must not look like one that
    /// ran and failed: a client deciding whether to retry needs the difference.
    #[test]
    fn task_failed_carries_the_run_reason_only_when_there_is_one() {
        let with_reason = serde_json::to_value(DaemonResponse::TaskFailed {
            task_id: "t1".to_string(),
            session_id: Some("s1".to_string()),
            error: "boom".to_string(),
            finish_reason: Some("Error".to_string()),
        })
        .expect("serializes");
        assert_eq!(with_reason["type"], "task_failed");
        assert_eq!(with_reason["finish_reason"], "Error");

        let without = serde_json::to_value(DaemonResponse::TaskFailed {
            task_id: "t1".to_string(),
            session_id: None,
            error: "could not start".to_string(),
            finish_reason: None,
        })
        .expect("serializes");
        assert!(
            without.get("finish_reason").is_none(),
            "omitted rather than null so old clients are unaffected"
        );
    }
}
