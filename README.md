# ⚡ Thunder Agent Loop

> **Ultra-lightweight, High-Performance, Minimal-Resource Agent Loop Engine in Rust**

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)]()

[English](README_en.md) | [简体中文](README.md)

---

## 🚀 核心特性

- **无上限自主循环 (Unbounded / Unlimited Loop)**：默认**不限制最大轮次**，完全由 LLM 意图和终止条件 (`finish_reason: "stop"` 或无工具调用) 决定终止，同时保留可选的轮次与 Token 预算熔断守卫。
- **极致性能与低延迟**：单任务调度开销仅 **~5.7 微秒 (0.0057 ms)**，吞吐量达 **170,000+ loops/sec**。
- **极小资源占用**：万级并发常驻内存 (RSS) **< 10 MB**，零垃圾回收抖动 (No GC)。
- **流式增量解析**：Zero-copy / Low-allocation SSE 流解析器，实时提取 Token Delta 与多 Tool Call 增量参数。
- **并行工具调度**：支持内存原生函数 (Native Tools) 与异步非阻塞子进程 (Bash)，内置超时熔断与 Head/Tail 智能输出尺寸截断。
- **自适应上下文裁剪**：支持混合策略（旧 Tool 输出压缩 + 滑动窗口），保障长对话中 Token 预算平稳与系统提示词（System Prompt）绝对置顶。
- **细粒度事件生命周期**：支持从 `TurnStart`、`TokenDelta`、`ToolCallChunk`、`ToolExecResult` 到 `LoopComplete` 的全生命周期可观测性广播。

---

## 📊 基准测试 (Benchmark)

在 Apple Silicon (M-series / ARM64) 环境下实测：

| 测试项 | 指标结果 | 说明 |
| :--- | :--- | :--- |
| **Token 估算吞吐量** | **1,465 MB/s** (15.3 亿字符/秒) | 零内存分配，中英双语自适应 |
| **万级并发调度能力** | **174,838 loops / sec** | 10,000 个并发 Agent Loop 在 57ms 内全部完成 |
| **单任务框架调度开销** | **5.72 µs (0.0057 ms)** | 排除外部网络时延后的真实纯净开销 |
| **10k 并发常驻内存 (RSS)**| **8.19 MB** | 内存占用极少，适合高密度微服务与边缘端嵌入 |

运行测试：
```bash
./test.sh bench
```

---

## 🛠️ 快速上手 (Quickstart)

### 1. 基本使用（默认无上限自主循环）

```rust
use std::sync::Arc;
use thunder_agent_loop::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化配置（默认无最大轮次限制）
    let config = AgentConfig::new("gpt-4o")
        .with_api_base("https://api.openai.com/v1")
        .with_api_key(std::env::var("OPENAI_API_KEY")?)
        .with_system_prompt("You are a high-speed AI assistant.")
        .with_unlimited_turns(); // 明确声明不限制循环轮次

    let mut agent = AgentLoop::new(config);
    
    // 注册内置工具
    agent.register_tool(Arc::new(BashTool::default())); // 异步 Bash
    agent.register_tool(Arc::new(ReadFileTool));         // 读文件
    agent.register_tool(Arc::new(WriteFileTool));        // 写文件

    // 2. 执行 Agent 循环（直到 LLM 完成任务或外部信号取消）
    let result = agent.run("Check system status and list current directory", None).await?;
    
    println!("Final Output: {:?}", result.final_content);
    println!("Total Turns: {}", result.stats.total_turns);
    println!("Tool Executions: {}", result.stats.total_tool_executions);

    Ok(())
}
```

---

### 2. 事件流订阅与实时可观测性

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
            AgentEvent::ToolExecResult { name, result, .. } => {
                println!("🛠️ [{}] 工具 '{}' 执行完成 ({}ms)", observed.agent_id, name, result.duration_ms);
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

### 3. 自定义工具开发

实现 `AgentTool` trait 即可扩展专属业务工具：

```rust
use async_trait::async_trait;
use serde_json::json;
use thunder_agent_loop::prelude::*;

pub struct CalculatorTool;

#[async_trait]
impl AgentTool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "calculate",
            "Perform mathematical calculations",
            json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "Math expression to evaluate, e.g. 12 * 45"
                    }
                },
                "required": ["expression"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let expr = args["expression"].as_str().ok_or("Missing expression")?;
        Ok(format!("Result of {}: 540", expr))
    }
}
```

---

### 4. 自适应上下文裁剪策略配置

```rust
use thunder_agent_loop::prelude::*;

let mut config = AgentConfig::new("gpt-4o");
config.pruning = ContextPruningConfig {
    max_context_tokens: 64_000,
    preserve_last_turns: 3,
    pin_system_prompt: true,
    strategy: PruningStrategy::Hybrid, // 压缩旧工具输出 + 滑动窗口
};
```

---

## 🧰 内置工具库

| 工具 | 注册名称 | 说明 |
| :--- | :--- | :--- |
| **`BashTool`** | `bash` | 异步非阻塞执行命令行，具备超时熔断与 Head/Tail 尺寸裁剪保护。 |
| **`ReadFileTool`** | `read_file` | 高性能异步文件读取，自动校验路径合法性。 |
| **`WriteFileTool`** | `write_file` | 异步原子写入文件，自动创建父级目录。 |

---

## 🧪 自动化测试脚本 (`test.sh`)

```bash
# 全流程自动化测试（单元测试 + 并发压测 + CLI 演示）
./test.sh

# 仅跑单元与集成测试
./test.sh test

# 跑 10,000 并发压测
./test.sh bench

# 交互式 CLI 运行
./test.sh run "查询当前目录并总结"
```

---

## 🧩 单元契约（A 是 Agent，B 是调度器）

本仓库是 **Agent A**：一只可独立跑完一生的单 Agent 单元。另一个 Rust 应用（**B**）可以依赖 A，创建多只 A 做编排。A 永远不依赖 B。详见 [ARCHITECTURE.md](ARCHITECTURE.md)。

```rust
// A 自己活完一轮任务
let mut agent = AgentLoop::new(config).with_id("researcher");
agent.register_tool(Arc::new(BashTool::default()));
let result = agent.run("Investigate the repo", None).await?;

// B（调度器）并排跑两只 A
let planner = AgentLoop::new(cfg.clone()).with_id("planner");
let coder = AgentLoop::new(cfg).with_id("coder");
let h1 = planner.start("Plan the change", None)?;
let h2 = coder.start("Implement it", None)?;
let (r1, r2) = tokio::try_join!(h1.join(), h2.join())?;
```

- 一只 `AgentLoop` 同时只跑一个任务；并行 = `new` 第二只。
- 事件带 `agent_id`，调度器可拆流。
- scratchpad 按 `agent_id` 隔离；默认不自动删除。
- 不安装全局 tracing subscriber；宿主（CLI 或 B）自己装。

---

## 📦 项目模块结构

```
src/
├── core/                  # 上下文缓冲 (Context Buffer)、状态追踪、Token 估算
├── loop_engine/           # 核心自主循环引擎、事件分发器
├── pruning/               # 自适应上下文裁剪（滑动窗口、工具输出压缩）
├── stream/                # 零拷贝 SSE 客户端与流解析器
├── tools/                 # 工具注册表、并行执行器与内置工具
│   └── builtin/           # bash, read_file, write_file
└── types/                 # 消息、事件、工具与配置类型定义
```

---

## 📄 开源协议

本项目采用 [Apache License 2.0](LICENSE) 许可证。
