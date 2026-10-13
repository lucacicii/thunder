# ⚡ Thunder Agent Root

> **Thunder Agent 微内核宿主框架与动态插件装配器**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-root` 是基于 [`thunder-agent-loop`](../thunder-agent-loop) 构建的微内核宿主。它负责将单 Agent 闭环引擎与多轮会话持久化、外部技能发现、MCP 远程工具、模型遥测无缝组装，对外提供统一的宿主级执行入口。

---

## 🚀 核心架构设计

1. **一切皆插件（Microkernel Architecture）**：
   - 宿主仅持有最小运行时调度能力，所有高级业务均以 `ThunderPlugin` 规范动态装配：
     - **`ConversationPlugin`**：多轮会话追踪、Turn 归纳与 Fs 原子持久化。
     - **`MemoryPlugin`**：分层加载 `.thunder` 长期记忆（`THUNDER.md` / `memory/*.md`），追加进系统提示。
     - **`SkillsPlugin`**：Playbook 技能库发现、Prompt 注入与动态加载工具。
     - **`McpPlugin`**：Model Context Protocol 远程服务连接与工具桥接。
     - **`MetricsPlugin`**：模型遥测——实时/平均 tok/s、token 明细、prompt cache 命中率、上下文占用与耗时；以 `metrics_update` 自定义事件随流推送，不侵入内核。
2. **确定性极速意图路由（Zero-LLM Latency Activation）**：
   - 彻底摒弃带来 1~2 秒延迟的二次 LLM 意图分类。
   - `PluginSelector` 确立**确定性基线**：会话、技能、记忆、网页与遥测插件对每次任务常驻激活；
   - 复杂扩展（如 MCP 服务器等）仅在用户 Prompt 命中特定意图关键词时精准激活，兼顾**零额外延迟、最小 Token 消耗与最强可扩展性**。
3. **全局技能扫描缓存（120s TTL）**：
   - 内置 `DEFAULT_SKILLS_CACHE`，对本地与全局技能目录文件树建立带 120 秒有效期的全局缓存，消除高频对话下的磁盘 I/O 阻塞。
4. **一键标准插件装配（`StandardHostBuilder`）**：
   - 开箱即用的装配入口，默认挂载会话、遥测、记忆、网页、技能与 MCP 标准插件；宿主专属能力（如 daemon 的 `ask_user`）在 `build` 之后用 `ThunderRoot::with_plugin` 追加。

---

## 🛠️ 快速上手

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::MemoryConversationStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_cfg = AgentConfig::new("deepseek-chat");

    // 标准插件集由 StandardHostBuilder 统一装配（会话 + 遥测 + 记忆 + 网页 + 技能 + MCP）
    let root = StandardHostBuilder::new(Arc::new(MemoryConversationStore::new()))
        .build(ThunderRoot::new(base_cfg));

    // 运行任务
    let options = RootRunOptions::default();
    let result = root.run("Check git status and summarize recent commits", options).await?;

    println!("Active Plugins: {:?}", result.selection.active_plugin_ids);
    println!("Final Content: {:?}", result.final_content);
    println!("Total Turns: {}", result.run_result.stats.total_turns);
    println!("Live tok/s: {:?}", result.metrics.map(|m| m.live_tps));

    Ok(())
}
```

---

## 🔌 核心插件一览

| 插件 ID | 插件名称 | 激活策略 | 核心能力 |
| :--- | :--- | :--- | :--- |
| **`conversation`** | 会话持久化 | **基线常驻** | 追踪多轮对话、更新 `index.json` 索引并执行原子落盘 |
| **`metrics`** | 模型遥测 | **基线常驻** | 跟踪实时/平均 tok/s（剔除 TTFT 与工具耗时）、token 明细、cache 命中率、上下文占用与总耗时；推送 `metrics_update` 快照 |
| **`skills`** | 技能库解析 | **基线常驻** | 扫描 `~/.agents/skills`，暴露 `load_skill` / `list_skills` 工具 |
| **`mcp`** | MCP 客户端 | 触发词动态激活 | 连接外部 MCP 服务器并暴露远程工具 |
| **`memory`** | 长期记忆 | **基线常驻** | 分层加载 `.thunder` 的 `THUNDER.md` / `memory/*.md`，追加进系统提示 |
| **`mode`** | 审批模式门 | **宿主显式安装** | 实现 `ToolGate`：`plan` / `ask` / `acceptEdits` / `yolo`，并携带 `write_plan` / `exit_plan_mode` |
| **`web`** | 网页工具 | **基线常驻** | `web_fetch`：抓取 URL、HTML 转 markdown，用 utility 模型按 prompt 提炼答案（无 utility 模型时回退 markdown）；`web_search`：走免 key 引擎（DuckDuckGo 主、Exa 兜底）返回标题/链接/摘要 |

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
