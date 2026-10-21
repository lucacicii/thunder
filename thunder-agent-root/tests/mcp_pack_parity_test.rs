//! P5: the same task, solved by the pack and by an MCP server.
//!
//! The seam's promise is that a tool is a tool: the loop calls `specs()` and
//! `call()`, and does not care who implements them. This test holds that
//! promise to a real task — write a file, read it back — once with the built-in
//! pack and once with an MCP server reached over stdio, and compares what the
//! user and the model actually see.
//!
//! The tool *names* differ (the bridge prefixes `mcp_<server>_`), and that is
//! the point: a model reads its tool table and adapts, while the task, the
//! arguments, the file that lands on disk and the shape of the result do not
//! change.

use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_mcp::config::{McpConfig, McpServerConfig};
use thunder_agent_mcp::manager::McpManager;
use thunder_agent_pack_code::builtin::fs::{ReadFileTool, WriteFileTool};
use thunder_agent_policy::registry_tools::plain_tools;
use thunder_agent_policy::AgentTool;
use tokio_util::sync::CancellationToken;

const CONTENT: &str = "parity-ok";

/// Answers every request in the shape the transport expects. The tools are the
/// ones the pack has, so the comparison is about the same operation, not two
/// different features that happen to look alike.
const FAKE_SERVER: &str = r#"#!/bin/sh
WS="$1"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"parity","version":"1.0.0"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"write_file","description":"Write content to a file.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}},{"name":"read_file","description":"Read a file.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"name":"write_file"'*)
          content=$(printf '%s' "$line" | sed -n 's/.*"content":"\([^"]*\)".*/\1/p')
          printf '%s' "$content" > "$WS/note.txt"
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"Wrote note.txt"}]}}\n' "$id" ;;
        *'"name":"read_file"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$(cat "$WS/note.txt")" ;;
      esac ;;
  esac
done
"#;

fn write_server(dir: &Path) -> PathBuf {
    let path = dir.join("parity-mcp-server.sh");
    std::fs::write(&path, FAKE_SERVER).expect("write server script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// The same two calls, whatever the tools are named.
struct ScriptedModel {
    write_tool: String,
    read_tool: String,
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
        let write_tool = self.write_tool.clone();
        let read_tool = self.read_tool.clone();
        tokio::spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            let (content, calls) = match turn {
                0 => (
                    "Writing.",
                    vec![ToolCall::new_function(
                        "c1",
                        write_tool,
                        json!({ "path": "note.txt", "content": CONTENT }).to_string(),
                    )],
                ),
                1 => (
                    "Reading.",
                    vec![ToolCall::new_function(
                        "c2",
                        read_tool,
                        json!({ "path": "note.txt" }).to_string(),
                    )],
                ),
                _ => ("Done.", vec![]),
            };
            let chunk = LLMStreamChunk::Completed {
                content: Some(content.to_string()),
                tool_calls: calls,
                finish_reason: if turn < 2 { "tool_calls" } else { "stop" }.to_string(),
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

struct Outcome {
    written: String,
    read: String,
    results: Vec<String>,
}

fn tool_results(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

async fn solve(ws: &Path, tools: Vec<Arc<dyn AgentTool>>, write: &str, read: &str) -> Outcome {
    let mut config = AgentConfig::new("test-model").with_max_turns(5);
    config.workspace_dir = Some(ws.to_path_buf());
    let agent = AgentLoop::new(config)
        .with_custom_client(Arc::new(ScriptedModel {
            write_tool: write.to_string(),
            read_tool: read.to_string(),
            turn: AtomicUsize::new(0),
        }))
        .with_tools(plain_tools(
            tools,
            64 * 1024,
            std::time::Duration::from_secs(10),
        ));

    let result = agent
        .run("write the note and read it back", None)
        .await
        .expect("run");
    assert_eq!(result.finish_reason, FinishReason::Done);

    let results = tool_results(&result.messages);
    let written = std::fs::read_to_string(ws.join("note.txt")).unwrap_or_default();
    let read = results.join("\n");
    Outcome {
        written,
        read,
        results,
    }
}

#[tokio::test]
async fn the_pack_and_an_mcp_server_solve_the_same_task_the_same_way() {
    // Two workspaces: the pack writes to one, the MCP server to the other, so
    // the comparison cannot be satisfied by one run seeing the other's file.
    let pack_dir = tempfile::tempdir().expect("tempdir");
    let mcp_dir = tempfile::tempdir().expect("tempdir");

    let pack_tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(WriteFileTool::default().with_default_cwd(pack_dir.path().to_path_buf())),
        Arc::new(ReadFileTool::default().with_default_cwd(pack_dir.path().to_path_buf())),
    ];
    let pack = solve(pack_dir.path(), pack_tools, "write_file", "read_file").await;

    let server_dir = tempfile::tempdir().expect("tempdir");
    let script = write_server(server_dir.path());
    let mut config = McpConfig::new();
    config.mcp_servers.insert(
        "parity".to_string(),
        McpServerConfig::new("sh")
            .with_arg(script.to_string_lossy().to_string())
            .with_arg(mcp_dir.path().to_string_lossy().to_string()),
    );
    let manager = McpManager::from_config(config).await.expect("manager");
    let mcp_tools = manager.discover_all_tools().await;
    let names: Vec<String> = mcp_tools
        .iter()
        .map(|t| AgentTool::definition(t.as_ref()).function.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "mcp_parity_write_file".to_string(),
            "mcp_parity_read_file".to_string()
        ],
        "the bridge must expose both tools under its prefix"
    );

    let mcp = solve(
        mcp_dir.path(),
        mcp_tools,
        "mcp_parity_write_file",
        "mcp_parity_read_file",
    )
    .await;
    manager.close_all().await;

    // What the user sees: the same file, with the same bytes.
    assert_eq!(pack.written, CONTENT, "the pack must write the note");
    assert_eq!(mcp.written, CONTENT, "the MCP server must write the note");
    assert_eq!(pack.written, mcp.written);

    // What the model sees: the same number of results, none of them errors,
    // and a read that returns the content it just wrote.
    assert_eq!(pack.results.len(), 2, "pack results: {:?}", pack.results);
    assert_eq!(mcp.results.len(), 2, "mcp results: {:?}", mcp.results);
    // A failed call would have left the second (read) result without the
    // content, which the two assertions below already catch.
    assert!(pack.read.contains(CONTENT), "pack read: {}", pack.read);
    assert!(mcp.read.contains(CONTENT), "mcp read: {}", mcp.read);
}
