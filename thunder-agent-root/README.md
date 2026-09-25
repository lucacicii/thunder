# ⚡ Thunder Agent Root

> **Thunder Agent 微内核宿主框架与动态插件装配器**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-root` 是基于 [`thunder-agent-loop`](../thunder-agent-loop) 构建的微内核宿主。它负责将单 Agent 闭环引擎与多轮会话持久化、外部技能发现、MCP 远程工具、TypeScript 脚本插件及多 Agent 编排流水线无缝组装，对外提供统一的宿主级执行入口。

---

## 🚀 核心架构设计

1. **一切皆插件（Microkernel Architecture）**：
   - 宿主仅持有最小运行时调度能力，所有高级业务均以 `ThunderPlugin` 规范动态装配：
     - **`ConversationPlugin`**：多轮会话追踪、Turn 归纳与 Fs 原子持久化。
     - **`SkillsPlugin`**：Playbook 技能库发现、Prompt 注入与动态加载工具。
     - **`McpPlugin`**：Model Context Protocol 远程服务连接与工具桥接。
     - **`ScriptPlugin`**：TypeScript 单文件无编译插件执行与热重载。
     - **`OrchestraPlugin`**：多 Agent 编排调度与子任务下钻委托。
2. **确定性极速意图路由（Zero-LLM Latency Activation）**：
   - 彻底摒弃带来 1~2 秒延迟的二次 LLM 意图分类。
   - `PluginSelector` 确立**确定性基线**：会话与技能插件对每次任务常驻激活；
   - 复杂扩展（如多 Agent 编排、MCP 服务器等）仅在用户 Prompt 命中特定意图关键词时精准激活，兼顾**零额外延迟、最小 Token 消耗与最强可扩展性**。
3. **全局技能扫描缓存（120s TTL）**：
   - 内置 `DEFAULT_SKILLS_CACHE`，对本地与全局技能目录文件树建立带 120 秒有效期的全局缓存，消除高频对话下的磁盘 I/O 阻塞。
4. **一键标准插件装配（`with_standard_plugins`）**：
   - 开箱即用的一键链式装配方法，默认挂载技能（Skills）与 MCP 标准插件；会话与编排插件可按需追加。

---

## 🛠️ 快速上手

```rust
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_cfg = AgentConfig::new("deepseek-chat");

    // 一键挂载标准插件集（技能、MCP），可再按需追加会话与编排插件
    let mut root = ThunderRoot::new(base_cfg)
        .with_standard_plugins()
        .with_plugin(ConversationPlugin::with_memory_store());

    // 运行任务
    let options = RootRunOptions::default();
    let result = root.run("Check git status and summarize recent commits", options).await?;

    println!("Active Plugins: {:?}", result.selection.active_plugin_ids);
    println!("Final Content: {:?}", result.final_content);
    println!("Total Turns: {}", result.run_result.stats.total_turns);

    Ok(())
}
```

---

## 🔌 核心插件一览

| 插件 ID | 插件名称 | 激活策略 | 核心能力 |
| :--- | :--- | :--- | :--- |
| **`conversation`** | 会话持久化 | **基线常驻** | 追踪多轮对话、更新 `index.json` 索引并执行原子落盘 |
| **`skills`** | 技能库解析 | **基线常驻** | 扫描 `~/.agents/skills`，暴露 `load_skill` / `list_skills` 工具 |
| **`mcp`** | MCP 客户端 | 触发词动态激活 | 连接外部 MCP 服务器并暴露远程工具 |
| **`script_plugin`** | TS 脚本引擎 | 触发词动态激活 | 原生执行单文件 TS 插件，支持蓝绿热重载与错误免疫 |
| **`orchestra`** | 多 Agent 调度 | 触发词动态激活 | 暴露 `delegate_subtask` 工具，支持子 Agent 隔离下钻 |

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
