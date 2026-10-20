//! The tool seam, end to end.
//!
//! These tests run a real [`AgentLoop`] with a host-supplied [`Tools`] and no
//! tools registered in the loop's own registry. They are the contract's proof:
//! the loop learns what exists from `specs()`, calls through `call()`, and never
//! touches the policy layer.

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::seam::{
    Concurrency, ToolCallContext, ToolOutcome, ToolSpec, Tools,
};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::ToolDefinition;
use tokio_util::sync::CancellationToken;

/// Answers with a tool call for the first `turns_with_calls` requests, then stops.
struct ScriptedLLM {
    seen: AtomicUsize,
    turns_with_calls: usize,
    /// Records the tool names each request carried.
    sent_tools: Mutex<Vec<Vec<String>>>,
}

impl ScriptedLLM {
    fn new(turns_with_calls: usize) -> Arc<Self> {
        Arc::new(Self {
            seen: AtomicUsize::new(0),
            turns_with_calls,
            sent_tools: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl LLMClientTrait for ScriptedLLM {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let names: Vec<String> = options
            .tools
            .iter()
            .map(|t| t.function.name.clone())
            .collect();
        self.sent_tools.lock().unwrap().push(names);

        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let index = self.seen.fetch_add(1, Ordering::SeqCst);
        let more = index < self.turns_with_calls;

        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                return;
            }
            let chunk = if more {
                LLMStreamChunk::Completed {
                    content: Some("working".to_string()),
                    // Deliberately a name that exists in NO registry.
                    tool_calls: vec![ToolCall::new_function(
                        format!("call_{index}"),
                        "phantom",
                        json!({ "n": index }).to_string(),
                    )],
                    finish_reason: "tool_calls".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(8),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else {
                LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(2),
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

struct PhantomTools {
    specs_calls: AtomicUsize,
    calls: Mutex<Vec<serde_json::Value>>,
    fail: bool,
}

impl PhantomTools {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            specs_calls: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            fail,
        })
    }
}

impl Tools for PhantomTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs_calls.fetch_add(1, Ordering::Relaxed);
        vec![ToolSpec::new(
            ToolDefinition::new_function("phantom", "a host-only tool", json!({"type": "object"})),
            Concurrency::Parallel,
        )]
    }

    fn call<'a>(
        &'a self,
        call: &'a ToolCall,
        _ctx: ToolCallContext<'a>,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let parsed: serde_json::Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
            self.calls.lock().unwrap().push(parsed);
            if self.fail {
                ToolOutcome::error("phantom refused")
            } else {
                ToolOutcome::success("phantom ran")
            }
        })
    }
}

fn config() -> AgentConfig {
    AgentConfig::new("test-model").with_max_turns(10)
}

#[tokio::test]
async fn a_host_supplied_tools_impl_replaces_the_registry_entirely() {
    let client = ScriptedLLM::new(2);
    let tools = PhantomTools::new(false);

    let agent = AgentLoop::new(config())
        .with_custom_client(client.clone())
        .with_tools(tools.clone());

    let result = agent.run("go", None).await.expect("run");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(tools.calls.lock().unwrap().len(), 2, "called once per turn");
    // Three turns: two with tool calls, one final stop. `specs()` is asked once
    // per turn, not once per tool call — the prefix is built per request.
    assert_eq!(tools.specs_calls.load(Ordering::Relaxed), 3, "specs once per turn");

    // The loop's own registry is empty: every request carried the host's spec.
    let sent = client.sent_tools.lock().unwrap();
    assert!(!sent.is_empty());
    for names in sent.iter() {
        assert_eq!(names, &vec!["phantom".to_string()], "only the host's tools");
    }
}

#[tokio::test]
async fn a_failing_tool_does_not_end_the_run() {
    let client = ScriptedLLM::new(1);
    let tools = PhantomTools::new(true);

    let agent = AgentLoop::new(config())
        .with_custom_client(client.clone())
        .with_tools(tools.clone());

    let result = agent.run("go", None).await.expect("run");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    // The error was fed back to the model as a tool result, not raised.
    assert!(result
        .final_content
        .as_deref()
        .unwrap_or_default()
        .contains("done"));
}

#[tokio::test]
async fn specs_are_the_only_thing_the_loop_asks_for() {
    let client = ScriptedLLM::new(0);
    let tools = PhantomTools::new(false);

    let agent = AgentLoop::new(config())
        .with_custom_client(client.clone())
        .with_tools(tools.clone());

    let result = agent.run("go", None).await.expect("run");
    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(tools.specs_calls.load(Ordering::Relaxed), 1);
    assert!(tools.calls.lock().unwrap().is_empty());

    let sent = client.sent_tools.lock().unwrap();
    assert_eq!(sent.first().map(|v| v.as_slice()), Some(&["phantom".to_string()][..]));
}
