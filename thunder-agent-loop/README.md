# ⚡ Thunder Agent Loop

> **Rust 编写的极轻量、高性能、高可观测性的单 Agent 闭环执行引擎**

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)]()

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-loop` 是 Thunder 生态中的核心执行单元（**Agent A**）。它专注于驱动单个 Agent 实例完成单次任务的完整生命周期（自主多轮推理、流式 Token 与思考链解析、并行工具调用、解耦上下文裁剪及状态落盘），并为外部编排调度器（**Agent B**，如 `thunder-orchestra`）提供高内聚、低耦合的嵌入底座。

---

## 🚀 核心特性

- **自主循环与安全兜底**：默认设定 **50 轮安全上限**，防止失控无限死循环；同时提供 `.with_unlimited_turns()` 显式解除限制，由 LLM 自主结论或完成标志（`finish_reason: "stop"` / 无工具调用）驱动终止。
- **纯契约传输解耦**：完全解耦 HTTP 与特定方言依赖，通过纯异步流式契约 `LLMClientTrait` 对接模型。生产环境无缝对接 `thunder-pi-bridge`（Node.js Sidecar 运行 `@earendil-works/pi-ai`）。
- **解耦工具裁剪（Tool Eviction）**：独创将**工具输出裁剪阈值**（`tool_eviction_threshold_tokens: 20_000`）与**模型物理上下文窗口**彻底解耦。当工具产出过多日志时自动压缩老旧工具结果，同时完整保留用户与 Assistant 的真实多轮对话至模型上下文上限（如 DeepSeek 1M tokens）。
- **跨 Agent 文件写入互斥排队锁（`FILE_MUTATION_LOCKS`）**：在 `TransactionMiddleware` 中内置进程内全局规范化路径排队锁。多个 Agent 或多工具并发写入同一文件时排队串行化执行，杜绝静默覆盖。
- **极致性能与低开销**：单任务调度框架纯净开销仅 **~5.7 微秒 (0.0057 ms)**，高并发常驻内存 (RSS) **< 10 MB**，零垃圾回收停顿 (No GC)。
- **原生并行工具调度**：支持异步非阻塞子进程（Bash）与内存函数（Native Tools），内置超时熔断与 Head/Tail 智能文本截断。
- **可观测性事件广播**：支持订阅从 `TurnStart`、`TokenDelta`、`ReasoningDelta`、`ToolExecResult` 到 `LoopComplete` 的全生命周期背压事件流。
- **权限分级与协作式暂停**：支持三档权限（`Read` ⊂ `Write` ⊂ `Bash`，未授权工具物理不注册），以及在工具调度边界生效的可恢复暂停（`PauseGate`）。

---

## 📊 基准测试 (Benchmark)

在 Apple Silicon (ARM64) 环境下实测：

| 测试项 | 指标结果 | 说明 |
| :--- | :--- | :--- |
| **Token 估算吞吐量** | **1,465 MB/s** (15.3 亿字符/秒) | 零内存分配，中英双语自适应 |
| **万级并发调度能力** | **174,838 loops / sec** | 10,000 个并发 Agent Loop 在 57ms 内全部完成 |
| **单任务框架调度开销** | **5.72 µs (0.0057 ms)** | 排除外部网络时延后的真实纯净调度开销 |
| **10k 并发常驻内存 (RSS)**| **8.19 MB** | 内存占用极少，适合高密度微服务与嵌入式运行 |

---

## 🛠️ 快速上手 (Quickstart)

### 1. 基础使用与工具注册

```rust
use std::sync::Arc;
use thunder_agent_loop::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化配置（默认 50 轮安全上限）
    let config = AgentConfig::new("gpt-4o")
        .with_system_prompt("You are a high-speed AI engineering assistant.")
        .with_max_turns(50); // 或 .with_unlimited_turns() 显式开启无上限

    let mut agent = AgentLoop::new(config);
    
    // 2. 注册所需工具
    agent.register_tool(Arc::new(BashTool::default()));
    agent.register_tool(Arc::new(ReadFileTool::default()));
    agent.register_tool(Arc::new(WriteFileTool::default()));

    // 3. 执行闭环任务
    let result = agent.run("Check system git status and summary the repository", None).await?;
    
    println!("Final Output: {:?}", result.final_content);
    println!("Total Turns: {}", result.stats.total_turns);
    println!("Tool Executions: {}", result.stats.total_tool_executions);

    Ok(())
}
```

---

### 2. 事件流订阅与流式输出

```rust
use thunder_agent_loop::prelude::*;

let mut agent = AgentLoop::new(config);
let mut event_rx = agent.subscribe_events();

tokio::spawn(async move {
    while let Ok(observed) = event_rx.recv().await {
        match observed.event {
            AgentEvent::TurnStart { turn, .. } => {
                println!("▶ [{}] 轮次 {} 开始", observed.agent_id, turn);
            }
            AgentEvent::TokenDelta { delta, .. } => {
                print!("{}", delta);
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                // 深度思考链（如 DeepSeek-R1 / o1）
                print!("[思考] {}", delta);
            }
            AgentEvent::ToolExecResult { name, result, .. } => {
                println!("🛠️ [{}] 工具 '{}' 完成 ({}ms)", observed.agent_id, name, result.duration_ms);
            }
            AgentEvent::TurnEnd { finish_reason, stats, .. } => {
                println!("⏹ 轮次结束 ({}, {}ms)", finish_reason, stats.duration_ms);
            }
            _ => {}
        }
    }
});
```

---

### 3. 解耦工具裁剪与上下文配置

```rust
use thunder_agent_loop::prelude::*;

let mut config = AgentConfig::new("deepseek-chat");
config.pruning = ContextPruningConfig {
    // 模型最大物理上下文（如 1,000,000 tokens）
    max_context_tokens: 1_000_000,
    // 工具输出解耦裁剪阈值：仅当历史 Tool 输出累计超过 20,000 tokens 时才触发旧输出压缩
    tool_eviction_threshold_tokens: 20_000,
    preserve_last_turns: 3,
    pin_system_prompt: true,
    strategy: PruningStrategy::Hybrid, // 混合策略：工具输出压缩 + 滑动窗口
};
```

---

### 4. 跨 Agent 文件并发写入互斥保护

当多个 Agent 单元或多个异步工具并发向同一文件发起写入时，`TransactionMiddleware` 会自动获取 `FILE_MUTATION_LOCKS`：

```rust
// 内部原理：进程内全局规范化路径互斥表
// 同一文件的并发写自动串行化，不同文件的并发写完全并行
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

// 写入流程：获取路径锁 → 写临时文件 → shadow rename 原子提交 → 释放锁并 best-effort 清理表项
let lock = FILE_MUTATION_LOCKS.lock().unwrap()
    .entry(normalized_path)
    .or_insert_with(|| Arc::new(TokioMutex::new(())))
    .clone();
let _guard = lock.lock().await;
```

**作用域与生命周期**：锁表是**进程内**的——同一进程内的所有 Agent、任务与工具（含 daemon 的全部 `run_task`）共享同一张表；**不跨进程**（TUI 与 daemon 同时写同一文件时无互斥保证）。每次无竞争写入完成后 best-effort 释放表项（有等待者时自动跳过），长跑进程不会无限膨胀。

---

## 🔒 权限档与协作式暂停

### `Permission`（能力档位）

```rust
pub enum Permission { Read, Write, Bash }   // 递进关系：Read ⊂ Write ⊂ Bash
```

挂载在 `AgentConfig` 上（默认 `Bash`）。它决定**宿主注册哪些内置工具**，未授予权限的工具在模型视图中**根本不可见**。底层 `PermissionGuardMiddleware` 提供双重防御阻断。

```rust
let cfg = AgentConfig::new("gpt-4o").with_permission(Permission::Read);
```

### `PauseGate`（协作式暂停）

`CancellationToken` 用于不可逆的任务取消；`PauseGate` 用于任务可恢复的协作式暂停。

```rust
let handle = agent.start("Long running task", None)?;
handle.pause();          // 在下一个工具调度边界安全挂起
handle.resume();         // 恢复执行
```

暂停检查点位于**工具派发之前**。当前执行中的工具会安全跑完，绝不在写入文件途中强行中断，杜绝文件损坏。

---

## 📂 模块结构

```text
src/
├── core/                  # 上下文缓冲 (Context Buffer)、状态追踪、Token 估算、PauseGate
├── loop_engine/           # 核心自主循环引擎、事件分发与任务状态机
├── pruning/               # 解耦上下文裁剪与工具日志驱逐策略
├── stream/                # LLMClientTrait 纯传输契约与 Mock 实现
├── tools/                 # 工具注册表、并行执行器与内置工具
│   ├── builtin/           # bash, read_file, write_file
│   └── middleware/        # 洋葱中间件（Security / Resource / Transaction / FILE_MUTATION_LOCKS）
└── types/                 # 消息、事件、工具与配置类型定义
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 开源许可证。
