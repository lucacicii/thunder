//! The approval dialog must name what is being approved.
//!
//! A gate computes `ApprovalPrompt { title, detail }` per call — the title
//! says the kind ("Run bash"), the detail says the substance (the command, the
//! path). The kernel used to forward only the title, so the user approved
//! blind. This file pins the whole path: gate detail → `UiRequest::Select.detail`
//! → the exact string the panel renders.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::gate::{ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{
    AgentTool, ToolDefinition, ToolExecutionContext, ToolExecutionResult,
};
use thunder_agent_loop::types::ui::{HostUi, NotifyLevel, UiRequest, UiResponse, UiSource};

struct BashLike;

#[async_trait]
impl AgentTool for BashLike {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "bash",
            "run a shell command",
            serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        Ok(format!("ran {}", args))
    }
}

/// Asks about everything, with a per-call detail like the mode gate builds.
struct AskEverything;

impl ToolGate for AskEverything {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        GateVerdict::Ask(ApprovalPrompt {
            title: format!("Run {}", request.tool),
            detail: thunder_agent_loop::types::policy::describe_call(request.arguments),
            allow_always: false,
        })
    }
    fn label(&self) -> &str {
        "ask-test"
    }
}

/// Records every request it sees, then denies (fail-closed like a real panel
/// would on "Deny").
struct CapturingUi {
    seen: Mutex<Vec<UiRequest>>,
}

impl CapturingUi {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl HostUi for CapturingUi {
    async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
        self.seen.lock().push(request);
        UiResponse::value("Deny")
    }
    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

#[tokio::test]
async fn approval_dialog_carries_what_is_being_approved() {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(BashLike));
    let ui = CapturingUi::new();
    let executor = ToolExecutor::new(registry)
        .with_tool_gate(Some(Arc::new(AskEverything)))
        .with_host_ui(Arc::clone(&ui) as Arc<dyn HostUi>);

    let ctx = ToolExecutionContext {
        tool_call_id: "call-1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let call = ToolCall::new_function("call-1", "bash", r#"{"command": "rm -rf /tmp/build"}"#);
    let result: ToolExecutionResult = executor.run_tool(&call, ctx, None).await;
    assert!(result.is_error, "the capturing UI denies everything");

    let seen = ui.seen.lock();
    assert_eq!(seen.len(), 1, "exactly one dialog: {seen:#?}");
    match &seen[0] {
        UiRequest::Select {
            title,
            detail,
            options,
            ..
        } => {
            assert_eq!(title, "Run bash");
            assert_eq!(
                detail.as_deref(),
                Some("rm -rf /tmp/build"),
                "the command must reach the dialog, not just the title"
            );
            assert!(options.iter().any(|o| o == "Deny"));
        }
        other => panic!("approval must be a Select dialog, got: {other:?}"),
    }
}
