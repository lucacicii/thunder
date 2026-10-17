# ⚡ Thunder Agent MCP

> **Bidirectional Model Context Protocol (MCP) Client & Server, Configuration Parser & Dynamic Tool Bridge for Thunder Agent**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-mcp` connects the Thunder ecosystem to the external Model Context Protocol (MCP) ecosystem. It supports connecting to external MCP server processes (client mode) to discover remote tools and adapt them into `thunder-agent-loop`'s `ToolPlugin`, as well as exporting local tools as an MCP server (server mode) for external clients like Claude Desktop or Cursor.

---

## 🚀 Key Features

1. **Bidirectional Standard MCP Protocol**:
   - Complete JSON-RPC 2.0 implementation aligned with MCP protocol version `2024-11-05`.
   - **Client**: Handshake initialization, dynamic tool discovery (`tools/list`), asynchronous remote tool invocation (`tools/call`), and `ping` health checks.
   - **Server (`McpServer`)**: Exposes local `ToolPlugin` implementations to any MCP-compliant client over Stdio or byte streams.
2. **Streaming Progress Notifications (`notifications/progress`)**:
   - Full support for `_meta.progressToken` parameter bindings.
   - Streams live `notifications/progress` events concurrently during long-running tool executions, returning final results upon completion.
3. **Multi-Source Configuration Parser**:
   - Parses standard `mcpServers` JSON configuration formats (e.g. `mcp_servers.json`, `claude_desktop_config.json`, `.thunder/config.json`).
4. **Stdio Transport Safety & Lifecycle Governance**:
   - Production-grade asynchronous stdio process communication holding `Child` handles to ensure server health, with clean `kill()` and PID recycling on close.
   - Per-request configurable timeout (default 120s) preventing hung child processes.
5. **Dual-Mode Tool Bridging (`McpToolBridge`)**:
   - Adapts remote tools into both `AgentTool` and `ToolPlugin`, with `discover_all_tool_plugins()` for seamless microkernel assembly.

---

## 🛠️ Quickstart

### 1. Client Mode: Connect to external MCP and mount tools

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
    // Discover tools ready to register into AgentLoop as ToolPlugin
    let plugins = manager.discover_all_tool_plugins().await;
    println!("Discovered {} MCP plugins", plugins.len());

    Ok(())
}
```

### 2. Server Mode: Export local ToolPlugins as an MCP Server

```rust
use thunder_agent_mcp::prelude::*;
use std::sync::Arc;

let server = McpServer::new("thunder-service", "0.6.0")
    .with_tool(my_tool_plugin);

// Serve over Stdio (e.g. for Claude Desktop or Cursor)
// server.serve(tokio::io::stdin(), tokio::io::stdout()).await?;
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
