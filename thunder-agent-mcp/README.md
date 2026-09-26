# ⚡ Thunder Agent MCP

> **Thunder Agent 的 Model Context Protocol (MCP) 客户端、配置解析与动态工具桥接**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-mcp` 将 Thunder 体系与标准 Model Context Protocol (MCP) 外部生态连接，支持自动解析标准服务配置、连接外部 MCP 服务进程、动态发现远程工具并将其无缝转换为 `thunder-agent-loop` 原生工具。

---

## 🚀 核心特性

1. **标准 MCP 协议支持**：
   - 完整实现 JSON-RPC 2.0 规范。
   - 标准初始化握手流程（`initialize` 与 `notifications/initialized`）。
   - 动态工具发现（`tools/list`）。
   - 远程工具异步调用（`tools/call`）。
2. **多源配置解析器**：
   - 兼容解析标准 `mcpServers` JSON 格式配置文件（如 `mcp_servers.json`、`claude_desktop_config.json`、`.arp/mcp.json` 等）。
3. **Stdio 传输 + 测试替身**：
   - 工业级异步 Stdio 子进程通信，支持管道独立隔离。
   - `MockTransport` 仅作为**测试替身**（纯内存、仅供单元与集成测试），不是运行时模式。
4. **零开销 AgentTool 桥接**：
   - 将远程 MCP 服务的工具定义自动映射为 Rust 端的 `AgentTool`，原生融入 `thunder-agent-loop` 的工具调度与洋葱中间件链路。

---

## 🛠️ 快速上手

```rust
use thunder_agent_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 装配 MCP 服务配置
    let mut config = McpConfig::new();
    config = config.with_server(
        "filesystem",
        McpServerConfig::new("npx")
            .with_arg("-y")
            .with_arg("@modelcontextprotocol/server-filesystem")
            .with_arg("/tmp"),
    );

    // 2. 初始化连接并发现工具
    let manager = McpManager::from_config(config).await?;
    let tools = manager.discover_all_tools().await;
    println!("Discovered {} MCP tools", tools.len());

    Ok(())
}
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
