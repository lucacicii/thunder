use serde_json::json;
use std::sync::Arc;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_mcp::prelude::*;
use tokio_util::sync::CancellationToken;

#[test]
fn test_mcp_config_parsing_standard_format() {
    let json_str = r#"{
        "mcpServers": {
            "fetch": {
                "command": "uvx",
                "args": ["mcp-server-fetch"],
                "env": { "HTTP_TIMEOUT": "30" }
            },
            "sqlite": {
                "command": "uvx",
                "args": ["mcp-server-sqlite", "--db-path", "test.db"],
                "disabled": true
            }
        }
    }"#;

    let cfg = McpConfig::parse_json(json_str).expect("Parse should succeed");
    assert_eq!(cfg.mcp_servers.len(), 2);

    let fetch = &cfg.mcp_servers["fetch"];
    assert_eq!(fetch.command, "uvx");
    assert_eq!(fetch.args, vec!["mcp-server-fetch"]);
    assert_eq!(
        fetch.env.get("HTTP_TIMEOUT").map(|s| s.as_str()),
        Some("30")
    );
    assert!(!fetch.disabled);

    let sqlite = &cfg.mcp_servers["sqlite"];
    assert!(sqlite.disabled);
}

#[tokio::test]
async fn test_mcp_client_with_mock_transport() {
    let mock_transport = Arc::new(MockTransport::new());
    let client = McpClient::from_transport("mock_server", mock_transport);

    // 1. Initialize
    let init_res = client
        .initialize(InitializeParams::default())
        .await
        .expect("Initialize should succeed");
    assert_eq!(init_res.server_info.name, "mock-mcp-server");

    // 2. Ping
    client.ping().await.expect("Ping should succeed");

    // 3. List tools
    let tools = client
        .list_tools()
        .await
        .expect("List tools should succeed");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "mock_fetch_data");

    // 4. Call tool
    let call_res = client
        .call_tool("mock_fetch_data", Some(json!({"query": "rust"})))
        .await
        .expect("Call tool should succeed");

    assert_eq!(call_res.plain_text(), "Mock response from mock_fetch_data");
}

#[tokio::test]
async fn test_mcp_tool_bridge_as_agent_tool() {
    let mock_transport = Arc::new(MockTransport::new());
    let client = Arc::new(McpClient::from_transport("test_srv", mock_transport));
    client
        .initialize(InitializeParams::default())
        .await
        .expect("Initialize must succeed");

    let tools = client.list_tools().await.expect("Tools list must succeed");
    let mcp_tool = tools
        .into_iter()
        .next()
        .expect("Should have at least one tool");

    let bridge = McpToolBridge::new("test_srv", mcp_tool, client);
    let def = bridge.definition();

    assert_eq!(def.function.name, "mcp_test_srv_mock_fetch_data");
    assert!(def.function.description.contains("[MCP:test_srv]"));

    let ctx = ToolExecutionContext {
        tool_call_id: "call_mcp_1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        ..Default::default()
    };

    let result = bridge
        .execute(json!({"query": "hello"}), &ctx)
        .await
        .expect("Tool bridge execution should succeed");

    assert!(result.contains("Mock response from mock_fetch_data"));
}

#[tokio::test]
async fn test_mcp_manager_discovery() {
    let manager = McpManager::new();

    let mock_transport = Arc::new(MockTransport::new());
    let client = McpClient::from_transport("database", mock_transport);
    client
        .initialize(InitializeParams::default())
        .await
        .expect("Init should succeed");

    manager.register_client(client).await;

    let discovered_tools = manager.discover_all_tools().await;
    assert_eq!(discovered_tools.len(), 1);
    assert_eq!(
        discovered_tools[0].definition().function.name,
        "mcp_database_mock_fetch_data"
    );
}

/// A server that answers `initialize` but hangs forever on `tools/list` must
/// not block tool discovery (and therefore the whole run). The per-server
/// timeout must skip it and still return the healthy servers' tools.
#[tokio::test]
async fn hanging_server_is_skipped_by_discovery_timeout() {
    use async_trait::async_trait;
    use thunder_agent_mcp::protocol::JsonRpcResponse;
    use thunder_agent_mcp::protocol::{JsonRpcNotification, JsonRpcRequest};

    /// Answers `initialize`, then never resolves another request.
    struct HangingServer;

    #[async_trait]
    impl McpTransport for HangingServer {
        async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
            if request.method == "initialize" {
                return Ok(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "hanging", "version": "1.0.0" }
                    })),
                    error: None,
                });
            }
            // Simulate a server that accepted the connection but wedged.
            std::future::pending::<()>().await;
            unreachable!()
        }

        async fn send_notification(
            &self,
            _notification: JsonRpcNotification,
        ) -> Result<(), McpError> {
            Ok(())
        }

        fn is_alive(&self) -> bool {
            true
        }

        async fn close(&self) -> Result<(), McpError> {
            Ok(())
        }
    }

    let manager = McpManager::new();
    let hanging = McpClient::from_transport("wedged", Arc::new(HangingServer));
    hanging
        .initialize(InitializeParams::default())
        .await
        .expect("initialize still answers");
    manager.register_client(hanging).await;

    let healthy = McpClient::from_transport("healthy", Arc::new(MockTransport::new()));
    healthy
        .initialize(InitializeParams::default())
        .await
        .expect("healthy initialize");
    manager.register_client(healthy).await;

    // 200ms discovery budget: the wedged server must be skipped, not awaited.
    let tools = manager
        .discover_all_tools_timeout(std::time::Duration::from_millis(200))
        .await;
    assert_eq!(
        tools.len(),
        1,
        "only the healthy server's tool is discovered"
    );
    assert_eq!(
        tools[0].definition().function.name,
        "mcp_healthy_mock_fetch_data"
    );
}
