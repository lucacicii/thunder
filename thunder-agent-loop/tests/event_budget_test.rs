//! KERNEL.md §8 anchor 12: the event stream is byte-bounded.
//!
//! The channel is bounded by frame count. These tests pin the second bound — the
//! one the count cannot give you, because one frame may be as large as a whole
//! tool result.

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::loop_engine::event_stream::EventBudget;
use thunder_agent_loop::seam::{Concurrency, ToolCallContext, ToolOutcome, ToolSpec, Tools};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_types::tool::ToolDefinition;
use tokio_util::sync::CancellationToken;

const BIG: usize = 64 * 1024;

/// Calls `bulk` once, then stops.
struct ScriptedLLM {
    seen: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for ScriptedLLM {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let first = self.seen.fetch_add(1, Ordering::SeqCst) == 0;
        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                return;
            }
            let chunk = if first {
                LLMStreamChunk::Completed {
                    content: Some("fetching".to_string()),
                    tool_calls: vec![ToolCall::new_function("c1", "bulk", "{}")],
                    finish_reason: "tool_calls".to_string(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else {
                LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            };
            let _ = tx.send(Ok(chunk)).await;
        });
        Ok(rx)
    }
}

/// Returns a result far larger than any sane event budget.
struct BulkTools;

impl Tools for BulkTools {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::new(
            ToolDefinition::new_function("bulk", "returns a lot", json!({"type": "object"})),
            Concurrency::Parallel,
        )]
    }

    fn call<'a>(
        &'a self,
        _call: &'a ToolCall,
        _ctx: ToolCallContext<'a>,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { ToolOutcome::success("x".repeat(BIG)) })
    }
}

/// Nothing a consumer receives may exceed the budget, and the loss must be
/// counted rather than silent.
#[tokio::test]
async fn an_event_larger_than_the_budget_never_reaches_the_stream() {
    let budget = 4096;
    let mut config = AgentConfig::new("test-model").with_max_turns(4);
    config.max_event_bytes = budget;
    // Keep the tool result large enough to matter but small enough to stay in
    // context; the event carries the same string.
    config.max_tool_output_bytes = BIG;

    let agent = AgentLoop::new(config)
        .with_custom_client(Arc::new(ScriptedLLM {
            seen: AtomicUsize::new(0),
        }))
        .with_tools(Arc::new(BulkTools));
    let drops = Arc::clone(agent.event_drop_counters());
    let mut handle = agent.start("go", None).expect("start");

    let mut events = handle.take_events().expect("stream");
    let mut biggest = 0usize;
    while let Some(event) = events.recv().await {
        biggest = biggest.max(event.approx_bytes());
    }
    let result = handle.join().await.expect("run");
    assert_eq!(result.finish_reason, FinishReason::Done);

    assert!(
        biggest <= budget,
        "no queued event may exceed the budget: saw {biggest} bytes"
    );
    assert!(
        drops.structural() > 0,
        "the oversized result must be counted as a lost structural event"
    );
}

/// The budget itself: what is reserved is not available twice, and what leaves
/// the queue is available again.
#[tokio::test]
async fn reserved_bytes_come_back_when_the_event_is_consumed() {
    let budget = Arc::new(EventBudget::new(1024));

    let held = budget.try_reserve(800).expect("fits");
    assert!(
        budget.try_reserve(400).is_none(),
        "a second reservation must not fit into the same bytes"
    );
    drop(held);
    assert!(
        budget.try_reserve(400).is_some(),
        "dropping the permit returns the space"
    );
}

/// One event larger than the whole budget can never fit, and the reservation
/// says so immediately instead of waiting out the deadline.
#[tokio::test]
async fn an_oversized_event_cannot_be_reserved_at_all() {
    let budget = Arc::new(EventBudget::new(1024));
    assert!(budget.try_reserve(2048).is_none());

    let started = std::time::Instant::now();
    assert!(budget.reserve(2048).await.is_none());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "an impossible reservation must fail fast, not wait for the deadline"
    );
}
