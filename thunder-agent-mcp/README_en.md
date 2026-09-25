# ⚡ Thunder Agent MCP

> **Model Context Protocol (MCP) Client, Configuration Parser & Dynamic Tool Bridge for Thunder Agent**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-mcp` connects the Thunder ecosystem to the external Model Context Protocol (MCP) ecosystem. It automatically parses standard server configurations, spawns MCP server processes, discovers remote tools dynamically, and seamlessly adapts them into native `thunder-agent-loop` tools.

---

## 🚀 Key Features

1. **Standard MCP Protocol Support**:
   - Complete JSON-RPC 2.0 implementation.
   - Standard initialization handshake (`initialize` + `notifications/initialized`).
   - Dynamic tool discovery (`tools/list`).
   - Asynchronous remote tool invocation (`tools/call`).
2. **Multi-Source Configuration Parser**:
   - Parses standard `mcpServers` JSON configuration formats (e.g. `mcp_servers.json`, `claude_desktop_config.json`, `.arp/mcp.json`).
3. **Stdio & Mock Dual Transports**:
   - Production-grade asynchronous stdio process communication with isolated pipes.
   - In-memory mock transport for deterministic unit and integration testing.
4. **Zero-Overhead AgentTool Bridging**:
   - Converts remote `McpTool` definitions into native Rust `AgentTool` instances, integrating directly with the `thunder-agent-loop` dispatch pipeline and onion middlewares.

---

## 🛠️ Quickstart

```rust
use thunder_agent_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure MCP server
    let mut config = McpConfig::new();
    config = config.with_server(
        "filesystem",
        McpServerConfig::new("npx")
            .with_arg("-y")
            .with_arg("@modelcontextprotocol/server-filesystem")
            .with_arg("/tmp"),
    );

    // 2. Connect and discover tools
    let manager = McpManager::from_config(config).await?;
    let tools = manager.discover_all_tools().await;
    println!("Discovered {} MCP tools", tools.len());

    Ok(())
}
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
