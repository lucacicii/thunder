# ⚡ Thunder Agent MCP

> **Model Context Protocol (MCP) Client, Configuration Parser & Dynamic Tool Bridge for Thunder Agent**

`thunder-agent-mcp` connects Thunder Agent units to external Model Context Protocol (MCP) servers, enabling dynamic discovery and invocation of remote tools.

## 🌟 Key Features

1. **Standard MCP Protocol**:
   - JSON-RPC 2.0 protocol implementation.
   - Handshake initialization (`initialize` + `notifications/initialized`).
   - Dynamic tool discovery (`tools/list`).
   - Remote tool invocation (`tools/call`).
2. **Configuration Parser**:
   - Parses standard `mcpServers` JSON config (`mcp_servers.json`, `claude_desktop_config.json`, `mcp.json`).
3. **Stdio & Mock Transports**:
   - Production-grade asynchronous stdio process communication with pipe isolation.
   - In-memory mock transport for deterministic testing.
4. **Zero-Overhead AgentTool Bridge**:
   - Seamlessly converts remote `McpTool` definitions into local `AgentTool` instances for `thunder-agent-loop`.

## 🚀 Quick Usage

```rust
use thunder_agent_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = McpConfig::new();
    config = config.with_server(
        "filesystem",
        McpServerConfig::new("npx")
            .with_arg("-y")
            .with_arg("@modelcontextprotocol/server-filesystem")
            .with_arg("/tmp"),
    );

    let manager = McpManager::from_config(config).await?;
    let tools = manager.discover_all_tools().await;
    println!("Discovered {} MCP tools", tools.len());

    Ok(())
}
```
