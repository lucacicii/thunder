use serde::{Deserialize, Serialize};
use serde_json::Value;
use thunder_protocol::ObservedEvent;
use thunder_providers::ModelDescriptor;

/// Revision of the NDJSON protocol on stdin/stdout expected by `agent-resume-panel`.
pub const PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderCapabilities {
    #[serde(default)]
    pub diagrams: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeChangeSource {
    User,
    PlanApproved,
    RunOption,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

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

/// Incoming command from host (Electron / CLI) via stdin.
#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum DaemonRequest {
    Ping {
        id: Option<String>,
    },
    ListModels {
        id: Option<String>,
    },
    ListConversations {
        id: Option<String>,
    },
    GetConversation {
        id: Option<String>,
        session_id: String,
    },
    RunTask {
        id: Option<String>,
        task_id: String,
        prompt: String,
        session_id: Option<String>,
        model: Option<String>,
        #[serde(default)]
        model_descriptor: Option<Box<ModelDescriptor>>,
        use_mock: Option<bool>,
        workspace_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(default)]
        extra_workspace_dirs: Option<Vec<String>>,
        thinking_level: Option<String>,
        #[serde(default)]
        attachments: Option<Vec<Attachment>>,
        #[serde(default)]
        headless: bool,
        #[serde(default, alias = "renderCapabilities")]
        render_capabilities: Option<RenderCapabilities>,
    },
    PauseTask {
        id: Option<String>,
        task_id: String,
    },
    ResumeTask {
        id: Option<String>,
        task_id: String,
    },
    SteerTask {
        id: Option<String>,
        task_id: String,
        message: String,
        behavior: Option<String>,
        #[serde(default)]
        attachments: Option<Vec<Attachment>>,
    },
    ClearQueue {
        id: Option<String>,
        task_id: String,
    },
    AnswerQuestion {
        id: Option<String>,
        question_id: String,
        #[serde(default)]
        answers: Value,
        #[serde(default)]
        cancelled: bool,
    },
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
    GetPermissionState {
        id: Option<String>,
        session_id: String,
    },
    SetMode {
        id: Option<String>,
        session_id: String,
        mode: String,
    },
    CancelTask {
        id: Option<String>,
        task_id: String,
    },
    ReloadIntegrations {
        id: Option<String>,
    },
    GetTrace {
        id: Option<String>,
        session_id: String,
        task_id: Option<String>,
    },
    ListTraces {
        id: Option<String>,
        session_id: String,
    },
    GenerateTitle {
        id: Option<String>,
        session_id: String,
        #[serde(default)]
        force: bool,
    },
    SetConversationTitle {
        id: Option<String>,
        session_id: String,
        title: String,
    },
    GetMetrics {
        id: Option<String>,
        session_id: String,
    },
    ListWorkspaces {
        id: Option<String>,
    },
    BindWorkspace {
        id: Option<String>,
        path: String,
        name: Option<String>,
        #[serde(default)]
        aliases: Vec<String>,
        #[serde(default)]
        keywords: Vec<String>,
        #[serde(default)]
        tech_stack: Vec<String>,
    },
    LocateWorkspace {
        id: Option<String>,
        prompt: String,
        workspace_dir: Option<String>,
    },
}

/// Outgoing message to host (Electron) via stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonResponse {
    Response {
        id: Option<String>,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ObservedEvent {
        task_id: String,
        event: ObservedEvent,
    },
    TaskCompleted {
        task_id: String,
        session_id: String,
        final_content: Option<String>,
        finish_reason: String,
        active_plugins: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggested_questions: Vec<String>,
    },
    SuggestedQuestions {
        task_id: String,
        session_id: Option<String>,
        questions: Vec<String>,
    },
    TaskFailed {
        task_id: String,
        session_id: Option<String>,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    UserQuestion {
        task_id: String,
        session_id: Option<String>,
        question_id: String,
        questions: Vec<QuestionItem>,
    },
    TaskPaused {
        task_id: String,
        session_id: Option<String>,
        reason: String,
    },
    TaskQueueUpdate {
        task_id: String,
        session_id: Option<String>,
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    ModeChanged {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous: Option<String>,
        source: ModeChangeSource,
    },
}

pub fn normalize_mode(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "yolo" | "bypass" | "bypasspermissions" => Some("yolo"),
        "plan" => Some("plan"),
        "ask" | "default" => Some("ask"),
        "acceptedits" | "accept_edits" | "autoedit" | "auto_edit" => Some("acceptEdits"),
        _ => None,
    }
}
