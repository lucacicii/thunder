//! P2: the reference pack, end to end through a real loop.
//!
//! The unit tests prove each tool in isolation. This one proves the thing a
//! user actually does: read a file, fix it, run the project's check, and stop
//! when the check passes — with the pack's real tools, a real executor and a
//! real `AgentLoop`, no host in between.

use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::ChatMessage;
use thunder_agent_pack_code::builtin::bash::BashTool;
use thunder_agent_pack_code::builtin::fs::{ReadFileTool, WriteFileTool};
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::registry_tools::StaticToolsFactory;
use tokio_util::sync::CancellationToken;

/// A model with a plan: read, write, run the check, stop.
struct ScriptedModel {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for ScriptedModel {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            let (content, calls) = match turn {
                0 => (
                    "Reading the greeting.",
                    vec![(
                        "c1",
                        "read_file",
                        json!({ "path": "greeting.txt" }).to_string(),
                    )],
                ),
                1 => (
                    "Fixing it.",
                    vec![(
                        "c2",
                        "write_file",
                        json!({ "path": "greeting.txt", "content": "hello world\n" }).to_string(),
                    )],
                ),
                2 => (
                    "Checking.",
                    vec![("c3", "bash", json!({ "command": "./check.sh" }).to_string())],
                ),
                _ => ("All good.", vec![]),
            };

            let chunk = LLMStreamChunk::Completed {
                content: Some(content.to_string()),
                tool_calls: calls
                    .into_iter()
                    .map(|(id, name, args)| {
                        thunder_agent_loop::types::message::ToolCall::new_function(id, name, args)
                    })
                    .collect(),
                finish_reason: if turn < 3 { "tool_calls" } else { "stop" }.to_string(),
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                cached_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            };
            let _ = tx.send(Ok(chunk)).await;
        });
        Ok(rx)
    }
}

fn tool_result_text(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// read → edit → run the check, with nothing but the reference pack installed.
#[tokio::test]
async fn the_pack_solves_a_real_task_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path();

    std::fs::write(ws.join("greeting.txt"), "hello there\n").unwrap();
    let script = ws.join("check.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ngrep -q 'hello world' greeting.txt && echo CHECK_OK\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The host's assembly: the pack's tools, one executor, one loop.
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(10));
    registry.register(Arc::new(
        ReadFileTool::default().with_default_cwd(ws.to_path_buf()),
    ));
    registry.register(Arc::new(
        WriteFileTool::default().with_default_cwd(ws.to_path_buf()),
    ));
    registry.register(Arc::new(
        BashTool::default().with_default_cwd(ws.to_path_buf()),
    ));
    let executor = ToolExecutor::new(registry.clone());

    let mut config = AgentConfig::new("test-model").with_max_turns(6);
    config.workspace_dir = Some(ws.to_path_buf());
    let agent = AgentLoop::new(config)
        .with_custom_client(Arc::new(ScriptedModel {
            turn: AtomicUsize::new(0),
        }))
        .with_tools_factory(Arc::new(StaticToolsFactory::new(registry, executor)));

    let result = agent.run("make the check pass", None).await.expect("run");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(
        std::fs::read_to_string(ws.join("greeting.txt")).unwrap(),
        "hello world\n",
        "the write must have landed on the real file"
    );

    let transcript = tool_result_text(&result.messages);
    assert!(
        transcript.contains("hello there"),
        "the read must have returned the file: {transcript}"
    );
    assert!(
        transcript.contains("CHECK_OK"),
        "the project's check must have run and passed: {transcript}"
    );
}

/// The program behind `bash` is a host decision; the tool keeps its name.
#[tokio::test]
async fn the_shell_program_can_be_pinned_by_the_host() {
    use thunder_agent_pack_code::builtin::shell::ShellConfig;

    let tool = BashTool::default().with_shell(ShellConfig::from_program("/bin/sh"));
    let ctx = thunder_agent_policy::ToolExecutionContext {
        tool_call_id: "c1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        ..Default::default()
    };

    let out =
        thunder_agent_policy::AgentTool::execute(&tool, json!({ "command": "echo $0" }), &ctx)
            .await
            .expect("execute");
    // `$0` inside `-c` names the program that ran the line.
    assert!(
        out.trim().ends_with("sh"),
        "the pinned program must be the one that ran: {out}"
    );
}
