//! Cross-crate seam tests: `OrchestraPlugin`'s delegate sub-agent path.
//!
//! `Scheduler::spawn_unit` injects the composition-root `ClientFactory`, but
//! `DelegateTool` builds its OWN `AgentLoop` inside the plugin's tool factory
//! and therefore bypasses the scheduler entirely. These tests lock the seam:
//! the plugin must forward its configured factory into every delegated
//! sub-agent, must fail loudly (not silently) without one, and must refuse to
//! register a delegate tool when no base config exists.

#![cfg(feature = "orchestra")]

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::{
    AgentConfig, ChatRequestOptions, LLMClientTrait, LLMStreamChunk, ToolExecutionContext,
};
use thunder_agent_root::prelude::*;
use thunder_orchestra::{ClientFactory, OrchestraConfig, Topology};
use tokio_util::sync::CancellationToken;

/// Counts `stream_chat` invocations and immediately completes with "ok".
struct CountingClient {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMClientTrait for CountingClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("ok".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(4),
                    completion_tokens: Some(2),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn exec_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "t1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    }
}

fn find_delegate(tools: &[Arc<dyn thunder_agent_loop::AgentTool>]) -> Arc<dyn thunder_agent_loop::AgentTool> {
    tools
        .iter()
        .find(|t| t.definition().function.name == "delegate_subtask")
        .expect("delegate_subtask tool must be registered")
        .clone()
}

#[tokio::test]
async fn delegate_subtask_streams_through_plugin_client_factory() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let factory: ClientFactory = Arc::new(move |_cfg: &AgentConfig| {
        Some(Arc::new(CountingClient { calls: counter.clone() }) as Arc<dyn LLMClientTrait>)
    });

    let cfg = OrchestraConfig::new(Topology::Auto)
        .with_base(AgentConfig::new("test-model"))
        .with_client_factory(factory);
    let plugin = OrchestraPlugin::new(cfg);

    let tools = plugin.tools();
    let delegate = find_delegate(&tools);

    let out = delegate
        .execute(
            serde_json::json!({ "task": "analyze this module for dead code" }),
            &exec_ctx(),
        )
        .await
        .expect("delegated sub-agent must complete through the factory client");

    assert_eq!(out, "ok");
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "sub-agent must have resolved its LLM client from the plugin's client factory"
    );
}

#[tokio::test]
async fn delegate_subtask_without_factory_fails_loudly() {
    let cfg = OrchestraConfig::new(Topology::Auto).with_base(AgentConfig::new("test-model"));
    let plugin = OrchestraPlugin::new(cfg);

    let tools = plugin.tools();
    let delegate = find_delegate(&tools);

    let msg = match delegate
        .execute(serde_json::json!({ "task": "anything" }), &exec_ctx())
        .await
    {
        Err(e) => e,
        Ok(out) => panic!(
            "client-less sub-agent must surface as a tool error, got Ok({:?})",
            out
        ),
    };
    assert!(
        msg.contains("client factory") || msg.contains("No LLM client"),
        "error must name the root cause, got: {msg}"
    );
}

#[tokio::test]
async fn no_base_config_disables_delegate_tool_instead_of_guessing() {
    let cfg = OrchestraConfig::new(Topology::Auto);
    let plugin = OrchestraPlugin::new(cfg);
    let tools = plugin.tools();
    assert!(
        tools.is_empty(),
        "without a base AgentConfig the delegate tool must not be registered (no phantom model)"
    );
}
