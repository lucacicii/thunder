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
