//! Cancelling a run must leave a conversation the user can continue.
//!
//! The failure this guards against is subtle and only shows up on the *next*
//! turn: a run stopped between "the model asked for tools" and "the tools ran"
//! leaves an assistant message whose calls have no results, and every provider
//! rejects that history. Pressing Esc at exactly that moment would lock the user
//! out of their own conversation.

use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio::sync::Notify;

/// Requests one tool on the first turn, then concludes.
struct ToolFirstClient {
    calls: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for ToolFirstClient {
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
                    "let me read that",
                    vec![
                        ToolCall::new_function("call_1", "probe", "{}"),
                        ToolCall::new_function("call_2", "probe", "{}"),
                    ],
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
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

/// Streams a little answer and then never finishes, so a cancel lands mid-turn.
struct MidStreamClient {
    entered: Arc<Notify>,
}

#[async_trait]
impl LLMClientTrait for MidStreamClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let entered = Arc::clone(&self.entered);
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Token("half an ".to_string())))
                .await;
            let _ = tx
                .send(Ok(LLMStreamChunk::Token("answer".to_string())))
                .await;
            entered.notify_waiters();
            // Never completes: the test cancels instead.
            cancel.cancelled().await;
        });
        Ok(rx)
    }
}

struct CountingTool {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentTool for CountingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("probe", "counts", json!({"type": "object"}))
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok("probed".to_string())
    }
}

/// Tool calls in the last assistant message that have no matching result.
fn unanswered(messages: &[ChatMessage]) -> Vec<String> {
    let Some(index) = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::Assistant { .. }))
    else {
        return Vec::new();
    };
    let answered: Vec<&str> = messages[index + 1..]
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    match &messages[index] {
        ChatMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } => calls
            .iter()
            .filter(|c| !answered.contains(&c.id.as_str()))
            .map(|c| c.id.clone())
            .collect(),
        _ => Vec::new(),
    }
}

/// Cancelling while parked at a tool boundary must not leave the model's tool
/// calls unanswered.
#[tokio::test]
async fn cancelling_at_a_tool_boundary_answers_the_tool_calls() {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut agent = AgentLoop::new(AgentConfig::new("mock").with_max_turns(4));
    agent.register_tool(Arc::new(CountingTool {
        runs: Arc::clone(&runs),
    }));
    agent = agent.with_custom_client(Arc::new(ToolFirstClient {
        calls: AtomicUsize::new(0),
    }));

    let mut events = agent.subscribe_events();
    let handle = agent.start("go", None).expect("start");

    // Park at the boundary, then cancel: the window in which the calls exist but
    // their results do not.
    handle.pause();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 0, "parked before any tool ran");
    handle.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("cancel releases the parked loop")
        .expect("join ok");

    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "the tools genuinely never ran"
    );
    assert!(
        unanswered(&result.messages).is_empty(),
        "every requested call must be answered: {:?}",
        unanswered(&result.messages)
    );
    let answered = result
        .messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(answered, 2, "both calls are answered");

    // The answer has to reach hosts that build their own history from events,
    // which is how the daemon keeps its per-session transcript.
    let mut reported = 0;
    while let Ok(observed) = events.try_recv() {
        if matches!(observed.event, AgentEvent::ToolExecResult { .. }) {
            reported += 1;
        }
    }
    assert_eq!(reported, 2, "hosts are told about both, not just the file");
}

/// Cancelling mid-stream keeps the part of the answer already shown.
#[tokio::test]
async fn cancelling_mid_stream_keeps_the_partial_answer() {
    let entered = Arc::new(Notify::new());
    let mut agent = AgentLoop::new(AgentConfig::new("mock").with_max_turns(4));
    agent = agent.with_custom_client(Arc::new(MidStreamClient {
        entered: Arc::clone(&entered),
    }));

    let handle = agent.start("go", None).expect("start");

    // Wait for the first tokens to be produced, then cut it off.
    let waiter = {
        let entered = Arc::clone(&entered);
        tokio::spawn(async move { entered.notified().await })
    };
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the client streamed something")
        .unwrap();
    handle.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("join")
        .expect("ok");

    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert_eq!(
        result.final_content.as_deref(),
        Some("half an answer"),
        "the host is handed the text the user already saw"
    );
    assert!(
        result.messages.iter().any(|m| matches!(
            m,
            ChatMessage::Assistant { content: Some(c), .. } if c == "half an answer"
        )),
        "and it is in the transcript the next turn is built from: {:?}",
        result.messages
    );
    assert!(unanswered(&result.messages).is_empty());
}
