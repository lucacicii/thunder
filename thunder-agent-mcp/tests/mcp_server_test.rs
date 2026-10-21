use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use thunder_agent_mcp::prelude::*;
use thunder_agent_policy::plugin::{CallToolResult, Plugin, ToolPlugin};
use thunder_agent_policy::ToolExecutionContext;
use thunder_agent_types::tool::ToolDefinition;
use tokio::io::AsyncWriteExt;

struct AddToolPlugin;

impl Plugin for AddToolPlugin {
    fn id(&self) -> &str {
        "add"
    }
}

#[async_trait]
impl ToolPlugin for AddToolPlugin {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "add",
            "Adds two numbers",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            }),
        )
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        let a = arguments.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = arguments.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sum = a + b;
        Ok(CallToolResult::success_text(format!("Result: {sum}")))
    }
}

#[tokio::test]
async fn test_mcp_server_initialize_and_tools_lifecycle() {
    let server = McpServer::new("test-server", "1.0.0").with_tool(Arc::new(AddToolPlugin));

    // 1. Test initialize
    let init_line = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }
    })
    .to_string();

    let init_resp = server.handle_raw_line(&init_line).await.expect("response");
    let init_json: serde_json::Value = serde_json::from_str(&init_resp).unwrap();
    assert_eq!(init_json["id"], 1);
    assert_eq!(init_json["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init_json["result"]["serverInfo"]["name"], "test-server");

    // 2. Test tools/list
    let list_line = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    })
    .to_string();

    let list_resp = server.handle_raw_line(&list_line).await.expect("response");
    let list_json: serde_json::Value = serde_json::from_str(&list_resp).unwrap();
    let tools = list_json["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "add");

    // 3. Test tools/call
    let call_line = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "add",
            "arguments": { "a": 15, "b": 27 }
        }
    })
    .to_string();

    let call_resp = server.handle_raw_line(&call_line).await.expect("response");
    let call_json: serde_json::Value = serde_json::from_str(&call_resp).unwrap();
    assert_eq!(call_json["id"], 3);
    assert_eq!(call_json["result"]["isError"], false);
    let content = &call_json["result"]["content"][0];
    assert_eq!(content["type"], "text");
    assert_eq!(content["text"], "Result: 42");
}

#[tokio::test]
async fn test_mcp_server_stream_serving() {
    let server = McpServer::new("test-stream", "1.0.0").with_tool(Arc::new(AddToolPlugin));

    let (client_write, server_read) = tokio::io::duplex(4096);
    let (server_write, mut client_read) = tokio::io::duplex(4096);

    // Spawn server task
    tokio::spawn(async move {
        let _ = server.serve(server_read, server_write).await;
    });

    let mut client_write = client_write;

    // Send tools/call over duplex
    let req = json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "add",
            "arguments": { "a": 100, "b": 200 }
        }
    })
    .to_string();

    client_write
        .write_all(format!("{req}\n").as_bytes())
        .await
        .unwrap();

    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(&mut client_read);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let resp_json: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp_json["id"], 100);
    assert_eq!(resp_json["result"]["content"][0]["text"], "Result: 300");
}

struct ProgressToolPlugin;

impl Plugin for ProgressToolPlugin {
    fn id(&self) -> &str {
        "progress_tool"
    }
}

#[async_trait]
impl ToolPlugin for ProgressToolPlugin {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "progress_tool",
            "Tool that reports progress",
            json!({ "type": "object" }),
        )
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        ctx.report_progress(1.0, Some(2.0), Some("Step 1 finished".to_string()))
            .await;
        ctx.report_progress(2.0, Some(2.0), Some("All done".to_string()))
            .await;
        Ok(CallToolResult::success_text("finished"))
    }
}

#[tokio::test]
async fn test_mcp_server_progress_streaming() {
    let server = McpServer::new("test-progress", "1.0.0").with_tool(Arc::new(ProgressToolPlugin));

    let (client_write, server_read) = tokio::io::duplex(4096);
    let (server_write, mut client_read) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let _ = server.serve(server_read, server_write).await;
    });

    let mut client_write = client_write;

    // Send tools/call with _meta.progressToken
    let req = json!({
        "jsonrpc": "2.0",
        "id": 200,
        "method": "tools/call",
        "params": {
            "name": "progress_tool",
            "_meta": {
                "progressToken": "token-test-123"
            }
        }
    })
    .to_string();

    client_write
        .write_all(format!("{req}\n").as_bytes())
        .await
        .unwrap();

    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(&mut client_read);

    // Read first line: notifications/progress
    let mut line1 = String::new();
    reader.read_line(&mut line1).await.unwrap();
    let note1: serde_json::Value = serde_json::from_str(&line1).expect("valid json note1");
    assert_eq!(note1["method"], "notifications/progress");
    assert_eq!(note1["params"]["progressToken"], "token-test-123");
    assert_eq!(note1["params"]["progress"], 1.0);
    assert_eq!(note1["params"]["total"], 2.0);
    assert_eq!(note1["params"]["message"], "Step 1 finished");

    // Read second line: notifications/progress
    let mut line2 = String::new();
    reader.read_line(&mut line2).await.unwrap();
    let note2: serde_json::Value = serde_json::from_str(&line2).expect("valid json note2");
    assert_eq!(note2["method"], "notifications/progress");
    assert_eq!(note2["params"]["progressToken"], "token-test-123");
    assert_eq!(note2["params"]["progress"], 2.0);
    assert_eq!(note2["params"]["message"], "All done");

    // Read third line: response for id 200
    let mut line3 = String::new();
    reader.read_line(&mut line3).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line3).expect("valid json resp");
    assert_eq!(resp["id"], 200);
    assert_eq!(resp["result"]["content"][0]["text"], "finished");
}
