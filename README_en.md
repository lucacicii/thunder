# ⚡ Thunder Agent Loop

> **Ultra-lightweight, High-Performance, Minimal-Resource Agent Loop Engine in Rust**

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)]()

[English](README_en.md) | [简体中文](README.md)

---

## 🚀 Core Features

- **Unbounded Autonomous Loop**: No hardcoded maximum turn limits by default. Termination is driven purely by LLM intent (`finish_reason: "stop"` or absence of tool calls) and external `CancellationToken`, while preserving optional turn and token budget circuit-breaker safeguards.
- **Extreme Performance & Ultra-Low Latency**: Pure scheduling framework overhead is only **~5.7 µs (0.0057 ms)** per task, achieving throughput exceeding **170,000+ loops/sec**.
- **Minimal Resource Footprint**: 10,000 concurrent agent loops consume **< 10 MB** of resident set size (RSS) memory, with zero garbage collection pauses (No GC).
- **Streaming Incremental SSE Parser**: Zero-copy / low-allocation Server-Sent Events (SSE) parser that extracts real-time token deltas and accumulates multi-tool-call arguments on the fly.
- **Parallel Asynchronous Tool Execution**: Executes native async Rust tools and non-blocking sub-processes (such as `BashTool`) concurrently, equipped with timeout circuit breakers and intelligent Head/Tail output truncation.
- **Adaptive Context Pruning**: Hybrid pruning strategies (old tool output truncation + sliding window) maintain long-horizon conversations within token budgets while keeping the system prompt permanently pinned.
- **Fine-Grained Lifecycle Observability**: Broadcasts comprehensive events across the entire loop lifecycle: `TurnStart`, `TokenDelta`, `ToolCallChunk`, `ToolCallReady`, `ToolExecResult`, `TurnEnd`, `LoopComplete`, and `Error`.

---

## 📊 Benchmark

Tested on Apple Silicon (ARM64 / M-series):

| Benchmark Item | Result Metric | Description |
| :--- | :--- | :--- |
| **Token Estimation Throughput** | **1,465 MB/s** (1.53 billion chars/sec) | Zero-allocation, bilingual adaptive estimation |
| **10k Concurrency Throughput** | **174,838 loops / sec** | 10,000 concurrent agent loops finished within 57 ms |
| **Single Task Framework Overhead** | **5.72 µs (0.0057 ms)** | Pure framework dispatch latency (excluding network I/O) |
| **10k Concurrency RSS Memory** | **8.19 MB** | Minimal footprint, ideal for high-density microservices and edge embeddings |

Run benchmarks locally:
```bash
./test.sh bench
```

---

## 🛠️ Quickstart

### 1. Basic Usage (Default Unbounded Autonomous Loop)

```rust
use std::sync::Arc;
use thunder_agent_loop::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize configuration (unbounded autonomous loop by default)
    let config = AgentConfig::new("gpt-4o")
        .with_api_base("https://api.openai.com/v1")
        .with_api_key(std::env::var("OPENAI_API_KEY")?)
        .with_system_prompt("You are a high-speed autonomous AI assistant.")
        .with_unlimited_turns(); // Explicitly declare unbounded execution

    let mut agent = AgentLoop::new(config);
    
    // Register built-in tools
    agent.register_tool(Arc::new(BashTool::default()));
    agent.register_tool(Arc::new(ReadFileTool));
    agent.register_tool(Arc::new(WriteFileTool));

    // 2. Execute agent loop (runs until task is fulfilled or cancelled)
    let result = agent.run("Check current system status and list files", None).await?;
    
    println!("Final Content: {:?}", result.final_content);
    println!("Total Turns: {}", result.stats.total_turns);
    println!("Tool Executions: {}", result.stats.total_tool_executions);

    Ok(())
}
```

---

### 2. Event Streaming & Real-Time Observability

Subscribe to live lifecycle events before running the loop:

```rust
use thunder_agent_loop::prelude::*;

let mut agent = AgentLoop::new(config);
let mut event_rx = agent.subscribe_events();

tokio::spawn(async move {
    while let Ok(event) = event_rx.recv().await {
        match event {
            AgentEvent::TurnStart { turn, .. } => {
                println!("▶ Turn {} started", turn);
            }
            AgentEvent::TokenDelta { delta, .. } => {
                print!("{}", delta);
            }
            AgentEvent::ToolExecResult { name, result, .. } => {
                println!("🛠️ Tool '{}' finished in {}ms", name, result.duration_ms);
            }
            AgentEvent::TurnEnd { finish_reason, stats, .. } => {
                println!("⏹ Turn ended ({}, {}ms)", finish_reason, stats.duration_ms);
            }
            _ => {}
        }
    }
});
```

---

### 3. Creating Custom Tools

Implement the `AgentTool` trait to register custom domain tools:

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
        // Process calculation...
        Ok(format!("Result of {}: 540", expr))
    }
}
```

---

### 4. Context Pruning Configuration

Configure adaptive context compaction to prevent prompt explosion on long-running loops:

```rust
use thunder_agent_loop::prelude::*;

let mut config = AgentConfig::new("gpt-4o");
config.pruning = ContextPruningConfig {
    max_context_tokens: 64_000,
    preserve_last_turns: 3,
    pin_system_prompt: true,
    strategy: PruningStrategy::Hybrid, // Truncate old tool outputs + sliding window
};
```

---

## 🧰 Built-in Tools

| Tool | Name | Description |
| :--- | :--- | :--- |
| **`BashTool`** | `bash` | Non-blocking async shell command execution with timeout & output truncation safeguards. |
| **`ReadFileTool`** | `read_file` | Fast async file reader with automatic path validation. |
| **`WriteFileTool`** | `write_file` | Async atomic file writer that automatically creates parent directories. |

---

## 🧪 Test & Automation Script (`test.sh`)

```bash
# Run full suite (Unit tests + 10k Concurrency Benchmark + CLI demo)
./test.sh

# Run unit and integration tests only
./test.sh test

# Run high-concurrency performance benchmark (10,000 concurrent loops)
./test.sh bench

# Run interactive CLI with streaming output
./test.sh run "Summarize directory structure using bash"
```

---

## 📦 Project Architecture

```
src/
├── core/                  # Context buffer, state tracker, token estimator
├── loop_engine/           # Main autonomous loop engine & event dispatcher
├── pruning/               # Adaptive sliding window & tool output compression
├── stream/                # Zero-copy SSE client & chunk parsers
├── tools/                 # Tool registry, parallel executor, built-in tools
│   └── builtin/           # bash, read_file, write_file
└── types/                 # Message, event, tool, and config data types
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
