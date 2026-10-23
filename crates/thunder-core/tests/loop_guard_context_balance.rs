//! Regression tests for the "every `tool_call` must have a paired tool message"
//! invariant (see `docs/agent-loop-budget-and-recovery.md` §7).
//!
//! The agent loop may abort mid-batch (loop guard repetition, loop guard circuit
//! breaker, cancellation). Before the fix, those paths returned without writing a
//! tool message for the calls they never executed, leaving an assistant message
//! with unpaired `tool_calls`. Providers reject that shape with HTTP 400 and the
//! daemon persists the context verbatim, so a single unpaired call poisons every
//! later run of the session.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, LlmTurnResponse, ReActAgentLoop, ScriptedMockDriver, ToolCallRequest,
};
use thunder_protocol::{
    ChatMessage, ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider,
};
use thunder_registry::ToolRegistry;
use tokio_util::sync::CancellationToken;

/// Provider whose `call_tool` behaviour is fully scripted by the test.
struct TestProvider {
    /// When false, every call fails with a non-zero exit.
    succeed: bool,
    /// When set, the token is cancelled as soon as the first call runs, so the
    /// loop observes a cancellation in the middle of the tool batch.
    cancel_on_call: Option<CancellationToken>,
    calls: std::sync::Mutex<usize>,
}

#[async_trait]
impl ToolProvider for TestProvider {
    fn provider_id(&self) -> &str {
        "test"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_mcp(
            "test",
            "echo",
            "Echo back the input",
            json!({ "type": "object", "properties": { "n": { "type": "number" } } }),
        )])
    }

    async fn call_tool(
        &self,
        _name: &str,
        arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        *self.calls.lock().unwrap() += 1;
        if let Some(token) = &self.cancel_on_call {
            token.cancel();
        }
        if self.succeed {
            Ok(format!("echo:{arguments}"))
        } else {
            Err(ToolExecutionError::NonZeroExit {
                code: 1,
                stderr: "boom".to_string(),
            })
        }
    }
}

fn loop_config(ws: &std::path::Path) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-balance".to_string(),
        workspace_cwd: ws.to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: thunder_core::ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(5),
        budget: thunder_core::LoopBudget {
            ..Default::default()
        },
        stall: thunder_core::StallConfig::default(),
        cache_warm_settings: None,
    }
}

/// Asserts that the message list is a valid provider payload: every assistant
/// `tool_calls` entry is followed by exactly one tool message with its id, in order.
fn assert_tool_calls_are_paired(messages: &[ChatMessage]) {
    let mut pending: Vec<String> = Vec::new();
    for msg in messages {
        match msg {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                assert!(
                    pending.is_empty(),
                    "assistant turn started while tool results were still pending: {pending:?}"
                );
                pending = calls.iter().map(|c| c.id.clone()).collect();
            }
            ChatMessage::Tool { tool_call_id, .. } if !pending.is_empty() => {
                assert_eq!(
                    pending.remove(0),
                    *tool_call_id,
                    "tool result arrived out of order"
                );
            }
            _ => {}
        }
    }
    assert!(
        pending.is_empty(),
        "unpaired tool_calls left in context: {pending:?}"
    );
}

fn echo_call(id: &str, n: u32) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        name: "mcp__test__echo".to_string(),
        arguments: json!({ "n": n }),
    }
}

#[tokio::test]
async fn repetition_abort_mid_batch_leaves_no_unpaired_tool_calls() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(TestProvider {
            succeed: true,
            cancel_on_call: None,
            calls: std::sync::Mutex::new(0),
        }))
        .await
        .unwrap();

    // Five *identical* calls in one turn: the loop guard's hard repetition limit
    // (5) fires on the fifth, so that call never executes.
    let calls: Vec<ToolCallRequest> = (0..5).map(|_| echo_call("rep-1", 1)).collect();
    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "repeating",
        calls,
    )]));

    let mut agent = ReActAgentLoop::new(loop_config(ws.path()), registry, driver);
    let err = agent.run("loop forever").await.expect_err("must abort");
    assert!(
        err.contains("repeated identically"),
        "expected a repetition abort, got: {err}"
    );

    let messages = agent.context().get_messages();
    assert_tool_calls_are_paired(&messages);
    let tool_results = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(
        tool_results, 5,
        "all five calls need a tool message (real or placeholder), got {tool_results}"
    );
}

#[tokio::test]
async fn circuit_break_mid_batch_leaves_no_unpaired_tool_calls() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(TestProvider {
            succeed: false,
            cancel_on_call: None,
            calls: std::sync::Mutex::new(0),
        }))
        .await
        .unwrap();

    // Six *distinct* failing calls: distinct arguments keep the repetition guard
    // out of the way so the consecutive-error breaker (5) is what fires.
    let calls: Vec<ToolCallRequest> = (0..6).map(|n| echo_call(&format!("brk-{n}"), n)).collect();
    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "failing", calls,
    )]));

    let mut agent = ReActAgentLoop::new(loop_config(ws.path()), registry, driver);
    let err = agent.run("fail repeatedly").await.expect_err("must abort");
    assert!(
        err.contains("consecutive tool execution errors"),
        "expected a circuit-break abort, got: {err}"
    );

    let messages = agent.context().get_messages();
    assert_tool_calls_are_paired(&messages);
    let tool_results = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(
        tool_results, 6,
        "the not-yet-executed sixth call needs a placeholder too, got {tool_results}"
    );
}

#[tokio::test]
async fn cancellation_mid_batch_leaves_no_unpaired_tool_calls() {
    let ws = tempdir().unwrap();
    let token = CancellationToken::new();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(TestProvider {
            succeed: true,
            // Cancel as soon as the first tool runs: the loop then sees a
            // cancelled token on the *second* call of the same batch.
            cancel_on_call: Some(token.clone()),
            calls: std::sync::Mutex::new(0),
        }))
        .await
        .unwrap();

    let calls: Vec<ToolCallRequest> = (0..4).map(|n| echo_call(&format!("cnl-{n}"), n)).collect();
    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "cancel me",
        calls,
    )]));

    let mut agent = ReActAgentLoop::new(loop_config(ws.path()), registry, driver)
        .with_cancellation_token(token);
    let err = agent.run("cancel mid batch").await.expect_err("must abort");
    assert!(
        err.to_lowercase().contains("cancel"),
        "expected a cancellation, got: {err}"
    );

    let messages = agent.context().get_messages();
    assert_tool_calls_are_paired(&messages);
    let tool_results = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(
        tool_results, 4,
        "the three calls after the cancelled one need placeholders, got {tool_results}"
    );
}

/// Sanity check that the helper itself detects an unbalanced payload, so the
/// three tests above cannot pass vacuously.
#[test]
fn pairing_assertion_detects_orphaned_tool_calls() {
    let messages = vec![
        ChatMessage::user("hi"),
        ChatMessage::assistant_turn(
            None,
            Some(vec![
                echo_call("a", 1).to_wire_tool_call(),
                echo_call("b", 2).to_wire_tool_call(),
            ]),
            None,
            None,
        ),
        ChatMessage::tool("a", "ok", Some("mcp__test__echo".to_string())),
    ];
    let result = std::panic::catch_unwind(|| assert_tool_calls_are_paired(&messages));
    assert!(result.is_err(), "orphaned tool_call must be detected");
}
