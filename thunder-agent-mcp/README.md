# ⚡ Thunder Agent MCP

> **Thunder Agent 的 Model Context Protocol (MCP) 双向客户端/服务端、配置解析与动态工具桥接**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-mcp` 将 Thunder 体系与标准 Model Context Protocol (MCP) 外部生态连接。既支持连接外部 MCP 服务进程（客户端模式）、动态发现远程工具并桥接为 `thunder-agent-loop` 的 `ToolPlugin`；又支持将本地工具集作为 MCP 服务导出（服务端模式），提供给 Claude Desktop、Cursor 等外部客户端调用。

---

## 🚀 核心特性

1. **标准双向 MCP 协议支持**：
   - 完整实现 JSON-RPC 2.0 规范与最新 2024-11-05 协议版本。
   - **客户端**：标准初始化握手、动态工具发现（`tools/list`）、远程异步调用（`tools/call`）与 `ping` 保活。
   - **服务端 (`McpServer`)**：把本地 `ToolPlugin` 实现通过标准 Stdio / 流式通道导出为 MCP Server，无缝服务外部生态。
2. **规范级流式进度上报 (`notifications/progress`)**：
   - 完整支持请求中的 `_meta.progressToken` 绑定。
   - 采用独立异步分发通道，在工具执行耗时任务时实时推送 `notifications/progress` 事件流，结果在工具完成后原子响应。
3. **多源配置解析器**：
   - 兼容解析标准 `mcpServers` JSON 格式配置文件（如 `mcp_servers.json`、`claude_desktop_config.json`、`.thunder/config.json` 等）。
4. **Stdio 传输安全与生命周期管理**：
   - 工业级异步 Stdio 子进程通信，持有 `Child` 句柄保证服务活跃，`close` 安全回收杜绝僵尸进程。
   - 每个调用具有默认 120s 超时保护（可独立配置），防止悬挂卡死。
5. **双模工具桥接 (`McpToolBridge`)**：
   - 自动映射为 `thunder-agent-loop` 的 `AgentTool` 与 `ToolPlugin`，支持 `discover_all_tool_plugins()` 供宿主和微内核动态装配。

---

## 🛠️ 快速上手

### 1. 客户端模式：连接外部 MCP 并挂载工具

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
    // 获取可直接注册进 AgentLoop 的 ToolPlugin 集合
    let plugins = manager.discover_all_tool_plugins().await;
    println!("Discovered {} MCP plugins", plugins.len());

    Ok(())
}
```

### 2. 服务端模式：将本地 ToolPlugin 作为 MCP Server 导出

```rust
use thunder_agent_mcp::prelude::*;
use std::sync::Arc;

let server = McpServer::new("thunder-service", "0.5.0")
    .with_tool(my_tool_plugin);

// 通过 Stdio 启动服务（对接 Claude Desktop 或 Cursor）
// server.serve(tokio::io::stdin(), tokio::io::stdout()).await?;
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
