# ⚡ Thunder Agent Loop

> **Rust 编写的极轻量、高性能、高可观测性的单 Agent 闭环执行引擎**

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)]()

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-loop` 是 Thunder 生态中的核心执行单元（**Agent 内核**）。它专注于驱动单个 Agent 实例完成单次任务的完整生命周期（自主多轮推理、流式 Token 与思考链解析、并行工具调用、解耦上下文裁剪及状态落盘），并为宿主层（`thunder-agent-root` / `thunder-agent-daemon` / `thunder-tui`）提供高内聚、低耦合的嵌入底座。

---

## 🚀 核心特性

- **自主循环与安全兜底**：默认设定 **50 轮安全上限**，防止失控无限死循环；同时提供 `.with_unlimited_turns()` 显式解除限制，由 LLM 自主结论或完成标志（`finish_reason: "stop"` / 无工具调用）驱动终止。
- **纯契约传输解耦**：完全解耦 HTTP 与特定方言依赖，通过纯异步流式契约 `LLMClientTrait` 对接模型。生产环境由 `thunder-agent-providers` 的 `RpiAiClient` 进程内直连模型（无子进程、无 Node 依赖）。
- **检查点式上下文压缩（Checkpoint Compaction）**：默认策略下，两次压缩之间请求前缀**字节级稳定**（供应商 Prompt Cache 全量命中）；仅当逼近模型真实窗口极限（`max_context_tokens - reserve_tokens`）时，才一次性将较旧历史交给 LLM 生成结构化检查点摘要（Goal / Progress / Key Decisions / Next Steps / Critical Context + 读写文件清单），尾部 `keep_recent_tokens` 原文保留。摘要请求禁用缓存写入（`cache_retention: "none"`）；摘要不可用时退化为**紧急裁剪**（原子地丢弃最老的完整轮次，绝不切断 tool call/result 配对）；压缩前原始轨迹经 `AgentRunResult.raw_messages` 保留。
- **一个工具执行入口**：全部工具调用（内建 / 插件 / MCP / runtime 远程）都经过 `ToolExecutor::run_tool`——取消 → 作用域 → 守卫 → 准入判定 → 弹窗 → 执行 → 落 trace。**没有洋葱**：判定顺序在代码里写死，不存在「决定之后、执行之前」这种可编排的位置。
- **极致性能与低开销**：单任务调度框架纯净开销仅 **~5.7 微秒 (0.0057 ms)**，高并发常驻内存 (RSS) **< 10 MB**，零垃圾回收停顿 (No GC)。
- **原生并行工具调度**：支持异步非阻塞子进程（Bash）与内存函数（Native Tools），内置超时熔断与 Head/Tail 智能文本截断。
- **可观测性事件广播**：支持订阅从 `TurnStart`、`TokenDelta`、`ReasoningDelta`、`ToolExecResult` 到 `LoopComplete` 的全生命周期背压事件流。
- **一轮即一条消息**：`ChatMessage::assistant_turn` 让宿主把一轮写成一条 assistant 消息——正文、本轮全部 `tool_calls`，外加 `reasoning`（该轮思维链）与 `stats`（轮次号 / token / 耗时）。后两者是**转录字段**：引擎自己的上下文里恒为 `None`，provider 转换只读 `content`/`tool_calls`，所以它们永不出现在请求里，而宿主是唯一能把思维链留住的一方（provider 只推一次）。
- **准入钩子与全量 trace**：内核只提供三个具名接缝——`ToolScope`（宿主回答「打到哪儿」）、`ToolGate`（宿主回答「准不准」）、`ToolTraceSink`（宿主收记录）；**不装 gate 即除守卫外全放行**。每次调用必然留下且只留下一条 trace，记录判定本身（`guard` / `gate` / `answer` / `scope`），落盘前统一脱敏。工具**始终全量注册**。

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
// 工具住在能力包里（`thunder-agent-pack-code`），不在内核（见第 4 节）。
use thunder_agent_pack_code::builtin::{BashTool, ReadFileTool, WriteFileTool};

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

### 3. 检查点式上下文压缩配置

```rust
use thunder_agent_loop::prelude::*;

let mut config = AgentConfig::new("deepseek-chat");
config.pruning = ContextPruningConfig {
    // 模型最大物理上下文（如 1,000,000 tokens）
    max_context_tokens: 1_000_000,
    // 触发阈值：estimated > max_context_tokens - reserve_tokens 时压缩
    reserve_tokens: 16_384,
    // 尾部保留原文的 token 预算（永不进入摘要）
    keep_recent_tokens: 20_000,
    // 可选：摘要专用小模型（默认复用主模型）
    summarizer_model: None,
    summarizer_max_tokens: 4096,
    pin_system_prompt: true,
};
```

---

### 4. 跨 Agent 文件并发写入互斥保护

写锁**不在内核里**：`FILE_MUTATION_LOCKS` 属于 `thunder-agent-pack-code/src/builtin/fs.rs` 的写工具——原子写是「写文件」这个能力自身的一部分，不是一个能从装配里漏掉的层。当多个 Agent 单元或多个异步工具并发向同一文件发起写入时，写工具会获取它：

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

### 准入：`ToolGate` / `ToolScope`

内核**不含任何审批意见**。它只提供两个钩子，以及一个不可绕过的守卫：

```rust
// 宿主回答「这次调用打到哪儿」——只返回分类，不返回裁决
pub trait ToolScope { fn scope_of(&self, tool: &str, effect: ToolEffect, args: &Value) -> Scope; }

// 宿主回答「准不准」——只能收紧；弹窗由内核负责
pub trait ToolGate {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict;
    fn record_answer(&self, request: &ToolGateRequest<'_>, answer: &ApprovalAnswer) {}
    fn label(&self) -> &str { "gate" }
}
```

**两者都不装时，除守卫外全部放行**——这是无头宿主的红线。审批模式（`plan` / `ask` / `acceptEdits` / `yolo`）由 `thunder-agent-root` 的 `ModePlugin` 实现，可卸载。

```rust
let agent = AgentLoop::new(cfg)
    .with_tool_scope(Arc::new(CodeScope::from_agent_config(&cfg, Some(&ws))))
    .with_tool_gate(Some(mode_plugin as Arc<dyn ToolGate>), Some(ui));
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
├── pruning/               # 检查点式上下文压缩（含紧急裁剪兜底）
├── stream/                # LLMClientTrait 纯传输契约（含未配置占位实现）
├── tools/                 # 工具注册表与并行执行器
│   ├── executor.rs        # run_tool：唯一执行入口（guard → gate → ask → execute → trace）
│   ├── guard.rs           # 内核守卫：危险命令（token 判定）+ 关键路径
│   ├── read_only.rs       # 只读命令判据：plan 模式下能证明什么都不改的 shell 免问
│   └── trace.rs           # ToolTrace、脱敏与 ToolTraceSink
└── types/                 # 消息、事件、工具与配置类型定义
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 开源许可证。
