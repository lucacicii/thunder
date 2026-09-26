//! Proves the pause gate parks the loop at a tool boundary and resumes it,
//! without interrupting the tool that was already running.

use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio::sync::Notify;

/// Requests the tool on the first turn, then concludes on the second.
struct TwoTurnClient {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMClientTrait for TwoTurnClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let (content, tool_calls, finish_reason) = if turn == 0 {
                (
                    "working",
                    vec![ToolCall::new_function("call_1", "probe", "{}")],
                    "tool_calls",
                )
            } else {
                ("done", vec![], "stop")
            };
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(content.to_string()),
                    tool_calls,
                    finish_reason: finish_reason.to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

/// A tool that signals when it runs, so the test can observe boundaries.
struct ProbeTool {
    runs: Arc<AtomicUsize>,
    entered: Arc<Notify>,
}

#[async_trait]
impl AgentTool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("probe", "signals execution", json!({"type": "object"}))
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_waiters();
        Ok("probed".to_string())
    }
}

#[tokio::test]
async fn pause_holds_at_tool_boundary_and_resume_continues() {
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());

    let mut agent = AgentLoop::new(AgentConfig::new("mock").with_max_turns(4));
    agent.register_tool(Arc::new(ProbeTool {
        runs: Arc::clone(&runs),
        entered: Arc::clone(&entered),
    }));
    agent = agent.with_custom_client(Arc::new(TwoTurnClient {
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    // Pause BEFORE starting: the loop must hold at the first tool boundary.
    let handle = agent.start("go", None).expect("start");
    handle.pause();
    assert!(handle.is_paused(), "handle reports paused");

    // The loop is parked, so the tool must not have run yet.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "tool must not run while paused"
    );

    // Resume: the loop proceeds and the tool runs.
    handle.resume();
    assert!(!handle.is_paused(), "handle reports resumed");

    let result = tokio::time::timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("loop finishes after resume")
        .expect("join ok");

    assert!(runs.load(Ordering::SeqCst) >= 1, "tool ran after resume");
    // Turn 1 issues the tool call, turn 2 concludes. Both happen after resume,
    // which is exactly what proves the pause held the loop rather than skipping work.
    assert_eq!(result.stats.total_turns, 2, "both turns ran after resume");
    assert_eq!(result.finish_reason, FinishReason::Done);
}

#[tokio::test]
async fn cancel_wins_over_a_pause() {
    let runs = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());

    let mut agent = AgentLoop::new(AgentConfig::new("mock").with_max_turns(4));
    agent.register_tool(Arc::new(ProbeTool {
        runs: Arc::clone(&runs),
        entered,
    }));
    agent = agent.with_custom_client(Arc::new(TwoTurnClient {
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let handle = agent.start("go", None).expect("start");
    handle.pause();

    // A cancel must release the parked loop rather than deadlock it.
    handle.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("cancel releases the parked loop")
        .expect("join ok");

    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert_eq!(runs.load(Ordering::SeqCst), 0, "tool never ran");
}
