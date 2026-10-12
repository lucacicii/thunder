//! With the tier gone, registration is not a capability decision: the built-ins
//! are always all there, and what a call may do is judged per call by the gate.
//!
//! This replaces the tier tests, which used to pin the opposite contract — that
//! a denied tool is never registered. The reason it flipped is D3: a tool
//! missing from the model's list is a tool the model cannot even ask to use,
//! and "cannot ask" is a worse failure than "asked and refused", because the
//! refusal carries a reason.
//!
//! The observation point is the event stream: a run started with a stub client
//! that immediately requests every tool reports them back through
//! `ToolExecStart`, so the list the *model* saw is what this test sees.

use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::ToolCallFunction;
use thunder_agent_root::host::{RootRunOptions, ThunderRoot};
use tokio_util::sync::CancellationToken;

/// Answers the first turn with one call to every built-in, then stops. The
/// model's own request is the most honest witness of what it could see.
struct AllToolsClient;

#[async_trait]
impl LLMClientTrait for AllToolsClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            // The built-ins, plus the plan tools the mode plugin carries: plan
            // mode's only exit is a tool, so it has to be in this list or the
            // mode is a room with no door.
            let tools = [
                "read_file",
                "write_file",
                "bash",
                "grep",
                "find",
                "ls",
                "write_plan",
                "exit_plan_mode",
            ];
            let tool_calls = tools
                .iter()
                .map(|name| thunder_agent_loop::types::message::ToolCall {
                    id: format!("c_{name}"),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: (*name).to_string(),
                        arguments: r#"{}"#.to_string(),
                    },
                })
                .collect();
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: None,
                    tool_calls,
                    finish_reason: "tool_calls".into(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn the_builtins_are_registered_on_every_run() {
    let base_cfg = AgentConfig::new("gpt-4o").with_max_turns(1);
    let root = ThunderRoot::new(base_cfg).with_workspace(std::env::temp_dir());

    let mut handle = root
        .execute(
            "list the tools you can use",
            RootRunOptions {
                custom_client: Some(Arc::new(AllToolsClient)),
                cancellation_token: Some(tokio_util::sync::CancellationToken::new()),
                ..Default::default()
            },
        )
        .await
        .expect("run starts");

    let mut rx = handle.take_events().expect("events");
    let mut seen = Vec::new();
    while let Some(event) = rx.recv().await {
        if let AgentEvent::ToolExecStart { name, .. } = event.event {
            seen.push(name);
        }
    }

    for tool in [
        "read_file",
        "write_file",
        "bash",
        "grep",
        "find",
        "ls",
        "write_plan",
        "exit_plan_mode",
    ] {
        assert!(
            seen.iter().any(|name| name == tool),
            "`{tool}` must be registered on every run; the gate decides what a call may do, not the list: {seen:?}"
        );
    }
}
