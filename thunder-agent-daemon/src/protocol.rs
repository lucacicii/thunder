use serde::{Deserialize, Serialize};
use thunder_agent_loop::types::event::ObservedEvent;

/// Incoming command from host (Electron / CLI) via stdin
#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum DaemonRequest {
    /// Ping / Heartbeat check
    Ping {
        id: Option<String>,
    },
    /// List all configured and available LLM models
    ListModels {
        id: Option<String>,
    },
    /// List stored conversations
    ListConversations {
        id: Option<String>,
    },
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
        thinking_level: Option<String>,
    },
    /// Cancel a running task by task_id
    CancelTask {
        id: Option<String>,
        task_id: String,
    },
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
}
