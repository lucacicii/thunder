//! End-to-end checkpoint compaction tests: a tiny context window forces the
//! pi-style compaction path; a mock client serves both the one-off
//! summarization call and the regular turns.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::pruning::checkpoint::{
    is_checkpoint_message, CHECKPOINT_MESSAGE_NAME, SUMMARIZATION_SYSTEM_PROMPT,
};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::{AgentEvent, FinishReason};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use tokio_util::sync::CancellationToken;

/// Mock client that answers summarization requests with a canned checkpoint
/// summary and normal turns with a tool-call turn followed by a final answer,
/// generating enough traffic to blow past the tiny test window.
struct CheckpointMockClient {
    turn_counter: AtomicUsize,
    summary_calls: AtomicUsize,
    /// When true, summarization requests fail → the pruner must degrade to
    /// the legacy mechanical compaction instead of stalling the run.
    fail_summaries: bool,
}

#[async_trait]
impl LLMClientTrait for CheckpointMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        let is_summary_request = options
            .messages
            .first()
            .and_then(|m| match m {
                ChatMessage::System { content, .. } => Some(content == SUMMARIZATION_SYSTEM_PROMPT),
                _ => None,
            })
            .unwrap_or(false)
            && options.tools.is_empty()
            && options.cache_retention.as_deref() == Some("none");

        if is_summary_request {
            self.summary_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_summaries {
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    let _ = tx.send(Err("summarizer unavailable".to_string())).await;
                });
                return Ok(rx);
            }
            let tool_calls_seen = options
                .messages
                .last()
                .map(|m| {
                    m.content_str()
                        .unwrap_or_default()
                        .matches("write_file")
                        .count()
                })
                .unwrap_or(0);
            let _ = tool_calls_seen;
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(
                            "## Goal\nFinish the compaction test\n\n## Progress\n### Done\n- [x] worked\n\n## Next Steps\n1. continue"
                                .to_string(),
                        ),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(50),
                        completion_tokens: Some(20),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            });
            return Ok(rx);
        }

        let count = self.turn_counter.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                let _ = tx.send(Err("Cancelled".to_string())).await;
                return;
            }

            if count < 6 {
                // Generate bulky tool traffic to inflate the context quickly.
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(format!("Working turn {count}...")),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{count}"),
                            "write_file",
                            format!(
                                "{{\"path\":\"f{count}.txt\",\"content\":\"{}\"}}",
                                "d".repeat(2000)
                            ),
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(30),
                        completion_tokens: Some(10),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("All done.".to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(30),
                        completion_tokens: Some(5),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

/// No-op write tool so tool results stay small but the assistant arguments
/// (which are counted in token estimates) inflate the context.
struct NoopWriteTool;

#[async_trait]
impl thunder_agent_loop::types::tool::AgentTool for NoopWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "write_file",
            "test write",
            serde_json::json!({"type":"object","properties":{}}),
        )
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

use thunder_agent_loop::types::tool::{ToolDefinition, ToolExecutionContext};

/// Tiny window + a throwaway workspace: the transaction middleware really
/// writes `write_file` targets, so tests must never touch the crate directory.
fn tiny_window_config(workspace: &std::path::Path) -> AgentConfig {
    let mut cfg = AgentConfig::new("mock-model")
        .with_max_turns(10)
        .with_workspace_dir(workspace);
    cfg.pruning.max_context_tokens = 1_600; // hard ceiling
    cfg.pruning.reserve_tokens = 600; // trigger at >1000 estimated tokens
    cfg.pruning.keep_recent_tokens = 200; // keep only the last exchange verbatim
    cfg
}

#[tokio::test]
async fn checkpoint_compaction_replaces_history_and_emits_event() {
    let client = Arc::new(CheckpointMockClient {
        turn_counter: AtomicUsize::new(0),
        summary_calls: AtomicUsize::new(0),
        fail_summaries: false,
    });

    let workspace = tempfile::tempdir().unwrap();
    let mut agent = AgentLoop::new(tiny_window_config(workspace.path())).with_id("ckpt_agent");
    agent = agent.with_custom_client(client.clone());
    agent.register_tool(Arc::new(NoopWriteTool));

    let mut handle = agent.start("please do the work", None).unwrap();
    let mut saw_compaction_event = false;
    if let Some(mut rx) = handle.take_events() {
        while let Some(observed) = rx.recv().await {
            if let AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                ..
            } = observed.event
            {
                saw_compaction_event = true;
                assert!(
                    tokens_after < tokens_before,
                    "compaction must shrink the context"
                );
            }
        }
    }
    let result = handle.join().await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert!(
        saw_compaction_event,
        "ContextCompacted event must be emitted"
    );
    assert!(
        client.summary_calls.load(Ordering::SeqCst) >= 1,
        "the summarization path must have been exercised"
    );

    // Projection: [system][checkpoint][kept tail...]
    let messages = &result.messages;
    assert!(messages.len() >= 2, "projection must exist");
    assert!(
        is_checkpoint_message(&messages[1]),
        "index 1 must hold the checkpoint message, got {:?}",
        messages[1]
    );
    if let ChatMessage::System { name, content, .. } = &messages[1] {
        assert_eq!(name.as_deref(), Some(CHECKPOINT_MESSAGE_NAME));
        assert!(
            content.contains("## Goal"),
            "structured summary format expected"
        );
    }
    // The raw pre-compaction transcript is preserved for hosts.
    let raw = result
        .raw_messages
        .as_ref()
        .expect("raw transcript preserved");
    assert!(
        raw.len() > messages.len(),
        "raw log must be larger than the projection"
    );
    assert!(
        raw.iter().any(|m| matches!(m, ChatMessage::User { .. })
            && m.content_str() == Some("please do the work")),
        "raw log retains the original user message"
    );
}

#[tokio::test]
async fn failed_summarization_falls_back_to_emergency_trim() {
    let client = Arc::new(CheckpointMockClient {
        turn_counter: AtomicUsize::new(0),
        summary_calls: AtomicUsize::new(0),
        fail_summaries: true,
    });

    let workspace = tempfile::tempdir().unwrap();
    let mut agent = AgentLoop::new(tiny_window_config(workspace.path())).with_id("ckpt_fallback");
    agent = agent.with_custom_client(client.clone());
    agent.register_tool(Arc::new(NoopWriteTool));

    let mut handle = agent.start("please do the work", None).unwrap();
    if let Some(mut rx) = handle.take_events() {
        while rx.recv().await.is_some() {}
    }
    let result = handle.join().await.unwrap();

    // The run completes despite the summarizer being down…
    assert_eq!(result.finish_reason, FinishReason::Done);
    assert!(
        client.summary_calls.load(Ordering::SeqCst) >= 1,
        "the failing summarization path must have been attempted"
    );
    // …and no checkpoint message was fabricated: the emergency trim (drop
    // oldest complete turns) bounded the context instead.
    let messages = &result.messages;
    if messages.len() >= 2 {
        assert!(
            !is_checkpoint_message(&messages[1]),
            "no checkpoint may be installed from a failed summary"
        );
    }
    // History was still bounded by the hard window (emergency trim ran).
    assert!(
        result.messages.len() < 30,
        "mechanical fallback must keep the history bounded"
    );
}
