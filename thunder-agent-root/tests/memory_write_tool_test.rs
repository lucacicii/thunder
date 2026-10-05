//! Coverage for the `memory_write` tool contributed by the memory plugin.
//!
//! Exercises the tool directly (no agent loop): the interesting contract is the
//! path jail and the append/overwrite behaviour, both of which are easy to
//! break silently.

#![cfg(feature = "conversation")]

use std::sync::Arc;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_root::plugins::memory::{MemoryPlugin, MEMORY_WRITE_TOOL};
use thunder_agent_root::prelude::*;

/// Bind a plugin to `ws` under a route and return its write tool, initialized.
async fn tool_for(ws: &std::path::Path) -> (MemoryPlugin, Arc<dyn AgentTool>) {
    let plugin = MemoryPlugin::new();
    let ctx = PluginContext::new("sess")
        .with_workspace(ws.to_path_buf())
        .with_route("r1");
    plugin.on_init(&ctx).await.unwrap();
    let tool = plugin.tools().first().cloned().unwrap();
    (plugin, tool)
}

fn run_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "c1".to_string(),
        turn: 1,
        route: Some("r1".to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn memory_write_appends_to_thunder_md_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let (_plugin, tool) = tool_for(ws).await;
    assert_eq!(tool.definition().function.name, MEMORY_WRITE_TOOL);

    let out = tool
        .execute(serde_json::json!({"content": "First note."}), &run_ctx())
        .await
        .expect("write should succeed");
    assert!(
        out.contains("THUNDER.md"),
        "result should name the file: {out}"
    );

    let out2 = tool
        .execute(serde_json::json!({"content": "Second note."}), &run_ctx())
        .await
        .unwrap();

    let body = std::fs::read_to_string(ws.join(".thunder").join("THUNDER.md")).unwrap();
    assert!(
        body.contains("First note."),
        "append lost the first note:\n{body}"
    );
    assert!(
        body.contains("Second note."),
        "append lost the second note:\n{body}"
    );
    let _ = out2;
}

#[tokio::test]
async fn memory_write_creates_topic_file() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let (_plugin, tool) = tool_for(ws).await;

    tool.execute(
        serde_json::json!({"content": "Build with ./test.sh.", "file": "memory/build.md"}),
        &run_ctx(),
    )
    .await
    .unwrap();

    let body =
        std::fs::read_to_string(ws.join(".thunder").join("memory").join("build.md")).unwrap();
    assert!(body.contains("./test.sh"));
}

#[tokio::test]
async fn memory_write_refuses_to_escape_the_jail() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let (_plugin, tool) = tool_for(ws).await;

    for bad in ["../escape.md", "/etc/passwd.md", "notes.txt", ""] {
        let res = tool
            .execute(serde_json::json!({"content": "x", "file": bad}), &run_ctx())
            .await;
        assert!(res.is_err(), "target `{bad}` should have been refused");
    }

    // Nothing may have been created outside `.thunder`.
    assert!(!temp.path().parent().unwrap().join("escape.md").exists());
}

#[tokio::test]
async fn memory_write_without_a_bound_workspace_is_refused() {
    let (_plugin, tool) = tool_for(std::path::Path::new("/tmp/ignored")).await;
    // A run context carrying no route cannot be resolved to a workspace.
    let ctx = ToolExecutionContext::default();
    let res = tool
        .execute(serde_json::json!({"content": "x"}), &ctx)
        .await;
    assert!(res.is_err());
}

#[tokio::test]
async fn overwrite_replaces_rather_than_appends() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let (_plugin, tool) = tool_for(ws).await;

    tool.execute(serde_json::json!({"content": "old"}), &run_ctx())
        .await
        .unwrap();
    tool.execute(
        serde_json::json!({"content": "new", "append": false}),
        &run_ctx(),
    )
    .await
    .unwrap();

    let body = std::fs::read_to_string(ws.join(".thunder").join("THUNDER.md")).unwrap();
    assert!(body.contains("new"));
    assert!(
        !body.contains("old"),
        "overwrite left the old content: {body}"
    );
}

// ── end-to-end: the route must flow from the run into the tool ──────────────

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

/// Asks for `memory_write` on turn 0, then finishes.
struct WriteRequestingClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for WriteRequestingClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let msg = if turn == 0 {
                LLMStreamChunk::Completed {
                    content: Some("Recording a note.".to_string()),
                    tool_calls: vec![ToolCall::new_function(
                        "c1",
                        MEMORY_WRITE_TOOL,
                        r#"{"content":"Remember: prefer write_file over shell redirection."}"#,
                    )],
                    finish_reason: "tool_calls".to_string(),
                    prompt_tokens: None,
                    completion_tokens: None,
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else {
                LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: None,
                    completion_tokens: None,
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            };
            let _ = tx.send(Ok(msg)).await;
        });
        Ok(rx)
    }
}

/// The whole point of the route key: a real run must reach the tool with the
/// workspace bound, and the write must land under `<ws>/.thunder/`.
#[tokio::test]
async fn memory_write_is_reachable_from_a_run() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    std::fs::create_dir_all(ws.join(".thunder")).unwrap();

    let root = StandardHostBuilder::new(Arc::new(
        thunder_conversation::prelude::MemoryConversationStore::new(),
    ))
    .build(
        ThunderRoot::new(thunder_agent_loop::prelude::AgentConfig::new("test/model"))
            .with_workspace(ws.to_path_buf()),
    );

    let mut handle = root
        .execute(
            "remember something",
            RootRunOptions {
                session_id: Some("mem_sess".to_string()),
                custom_client: Some(Arc::new(WriteRequestingClient {
                    turn: AtomicUsize::new(0),
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    if let Some(mut rx) = handle.take_events() {
        while rx.recv().await.is_some() {}
    }
    let _ = handle.join().await.unwrap();

    let path = ws.join(".thunder").join("THUNDER.md");
    assert!(path.exists(), "memory_write did not reach the file system");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("prefer write_file over shell redirection"),
        "note not persisted: {body}"
    );
}
