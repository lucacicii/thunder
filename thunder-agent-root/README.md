# ⚡ Thunder Agent Root

> **Extensible Microkernel Host & Dynamic Plugin Orchestrator for Thunder Agent**

`thunder-agent-root` 是基于 [`thunder-agent-loop`](../thunder-agent-loop) 构建的微内核宿主框架。

## 🌟 核心理念

1. **AgentLoop 为核心底座**：底层单 Agent 闭环驱动保持纯粹、稳定与高性能。
2. **一切皆插件（Plugin-Driven）**：
   - 会话持久化与上下文追踪（`ConversationPlugin`）
   - 多 Agent 编排流水线（`OrchestraPlugin`）
   - 外部技能解析与 Prompt 注入（`SkillsPlugin`）
   - 模型上下文协议工具发现（`McpPlugin`）
   - 交互终端（`thunder-tui`）
3. **自主意图驱动（Dynamic Plugin Activation）**：根据用户任务特征，由 `PluginSelector` 自主决定动态挂载激活哪些插件与工具，兼顾最小 Token 消耗与强扩展性。

## 🚀 快速使用

```rust
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_cfg = AgentConfig::new("gpt-4o");
    let mut root = ThunderRoot::new(base_cfg)
        .with_plugin(ConversationPlugin::with_memory_store())
        .with_plugin(SkillsPlugin::default())
        .with_plugin(McpPlugin::default());

    let result = root.run("Review the codebase for memory leaks", RootRunOptions::default()).await?;
    println!("Selected plugins: {:?}", result.selection.active_plugin_ids);
    println!("Final response: {:?}", result.final_content);

    Ok(())
}
```
