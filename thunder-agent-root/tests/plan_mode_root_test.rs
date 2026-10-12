//! Plan mode through the real host wiring: the mode plugin judges the calls,
//! and the tools it carries are the only way out.
//!
//! `plan_mode_test.rs` drives the gate and the tools directly. This file drives
//! them the way a run does — `ThunderRoot` assembling a session — because the
//! two are separate wiring steps and only one of them is about the mode: the
//! plugin provides the gate *and* contributes tools, and a host that installs
//! the first without the second gives the model a mode with no exit.

use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::ToolCallFunction;
use thunder_agent_root::host::{RootRunOptions, ThunderRoot};
use thunder_agent_root::plugins::mode::{Mode, ModePlugin, WRITE_PLAN_TOOL};
use tokio_util::sync::CancellationToken;

/// Asks to write the plan, then to write an ordinary file, then stops. The
/// second call is the one plan mode has to refuse.
struct PlanThenWriteClient;

#[async_trait]
impl LLMClientTrait for PlanThenWriteClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let calls = vec![
                thunder_agent_loop::types::message::ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: WRITE_PLAN_TOOL.to_string(),
                        arguments: serde_json::json!({"content": "# Plan\n\n1. read, then write"})
                            .to_string(),
                    },
                },
                thunder_agent_loop::types::message::ToolCall {
                    id: "c2".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "write_file".to_string(),
                        arguments: serde_json::json!({"path": "out.txt", "content": "hi"})
                            .to_string(),
                    },
                },
            ];
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: None,
                    tool_calls: calls,
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
async fn a_plan_mode_run_can_write_its_plan_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());

    let plugin = ModePlugin::new(Mode::Plan);
    let root =
        ThunderRoot::new(AgentConfig::new("gpt-4o").with_max_turns(2)).with_workspace(ws.clone());

    let mut handle = root
        .execute(
            "plan the change",
            RootRunOptions {
                custom_client: Some(Arc::new(PlanThenWriteClient)),
                cancellation_token: Some(CancellationToken::new()),
                mode: Mode::Plan,
                mode_plugin: Some(Arc::clone(&plugin)),
                ..Default::default()
            },
        )
        .await
        .expect("run starts");

    let mut rx = handle.take_events().expect("events");
    let mut results = Vec::new();
    while let Some(event) = rx.recv().await {
        if let AgentEvent::ToolExecResult { name, result, .. } = event.event {
            results.push((name, result.is_error, result.output));
        }
    }

    let plan_call = results
        .iter()
        .find(|(name, ..)| name == WRITE_PLAN_TOOL)
        .expect("the plan tool must be reachable from a real run");
    assert!(
        !plan_call.1,
        "writing the plan is the one write plan mode allows: {}",
        plan_call.2
    );
    assert!(
        ws.join(".thunder").join("plans").exists(),
        "the plan landed under the session's workspace"
    );

    let write_call = results
        .iter()
        .find(|(name, ..)| name == "write_file")
        .expect("the model sees write_file, which is the whole point of D3");
    assert!(
        write_call.1,
        "plan mode refuses the write: {}",
        write_call.2
    );
    assert!(
        write_call.2.contains("exit_plan_mode"),
        "and names the way out: {}",
        write_call.2
    );
    assert!(!ws.join("out.txt").exists(), "nothing was written");
}

/// The mode the run is judged in survives the round trip through the host: a
/// plugin passed in `plan` is the plugin that judges, not a fresh default.
#[tokio::test]
async fn the_host_honours_the_mode_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = ModePlugin::new(Mode::Yolo);
    let root = ThunderRoot::new(AgentConfig::new("gpt-4o").with_max_turns(1))
        .with_workspace(dir.path().to_path_buf());

    let handle = root
        .execute(
            "say hi",
            RootRunOptions {
                custom_client: Some(Arc::new(PlanThenWriteClient)),
                cancellation_token: Some(CancellationToken::new()),
                mode: Mode::Plan,
                mode_plugin: Some(Arc::clone(&plugin)),
                ..Default::default()
            },
        )
        .await
        .expect("run starts");
    let _ = handle.join().await;

    assert_eq!(
        plugin.mode(),
        Mode::Plan,
        "the run's mode, not the plugin's previous one"
    );
}
