# ⚡ Thunder Conversation

> **高性能、分层拓扑感知的会话与 Session 管理子包**

[English](README_en.md) | [简体中文](README.md)

`thunder-conversation` 是 Thunder Agent 生态中的核心会话管理模块。它负责管理单 Agent 与多 Agent 编排任务的会话生命周期、Turn 轮次聚合、上下文转换及分层持久化。

---

## 🌟 核心特性

- **多编排拓扑原生感知**：
  - **Sequential (串行流水线)**：阶段切片 (`StageRecord`)，精确追踪 `planner` → `coder` 各阶段交接产出。
  - **Parallel (并行分治)**：会话派生分支 (`create_branch`) 与多视角合并归约 (`merge_parallel_into_parent`)。
  - **Delegate (嵌套委托)**：工具调用关联子会话 (`record_delegated_task`)，实现父会话干净收敛、子会话自由下钻。
- **双模存储架构**：
  - **`MemoryConversationStore`**：基于 `RwLock<HashMap>` 的线程安全纯内存存储，适合单次运行、测试与低延迟缓存。
  - **`FsConversationStore`**：基于文件系统与原子写入（`rename`）的持久化存储，内置 `index.json` 极速索引缓存，规避大文件列表遍历开销。
- **Turn 轮次抽象与智能裁剪**：
  - 自动将 `User -> Assistant -> Tool -> Assistant` 聚合为逻辑轮次。
  - 支持按轮次裁剪 (`truncate_turns`) 并保持 System Prompt 绝对置顶。
- **无缝桥接运行时**：
  - 会话可直接转换为 `thunder-agent-loop` 的 `ContextInput` 或 `ContextBuffer`。
- **全格式导出**：
  - 支持导出结构化 Markdown、标准 JSON 与 OpenAI 兼容消息数组。

---

## 🚀 快速上手

```rust
use thunder_conversation::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化持久化存储（也可使用 MemoryConversationStore）
    let store = FsConversationStore::new("~/.thunder/conversations").await?;
    let manager = ConversationManager::new(store);

    // 2. 创建并配置会话
    let mut conv = manager
        .create_with_prompt(
            "session_001",
            Some("系统设计讨论".to_string()),
            Some("You are a senior system architect.".to_string()),
        )
        .await?;

    // 3. 追加用户消息
    conv.add_user_message("如何设计高吞吐的 Agent 循环引擎？");
    manager.save(&conv).await?;

    // 4. 将会话转为 Agent 输入
    let context_input = conv.as_context_input();

    // 5. 导出 Markdown 排版
    let md = ConversationExporter::to_markdown(&conv);
    println!("{md}");

    Ok(())
}
```

---

## 🧪 自动化测试

```bash
# 运行子包全套单元与集成测试
./test.sh
```
