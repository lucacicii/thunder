# ⚡ Thunder Agent Loop

> **Ultra-lightweight, High-Performance, Minimal-Resource Single-Agent Loop Engine in Rust**

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-passing-brightgreen.svg)]()

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-loop` is the core atomic execution unit (**Agent Kernel**) in the Thunder ecosystem. It drives the complete autonomous lifecycle of a single agent instance on a single task (multi-turn reasoning, streaming token/reasoning parsing, concurrent tool execution, decoupled context eviction, and state persistence), serving as a clean, cohesive substrate for host layers (`thunder-agent-root` / `thunder-agent-daemon` / `thunder-tui`).

---

## 🚀 Key Features

- **Bounded Autonomous Loop**: Configured with a default **50 turns ceiling** to safeguard against infinite loops, paired with an explicit `.with_unlimited_turns()` escape hatch where termination is driven purely by model completion (`finish_reason: "stop"` or absence of tool calls).
- **Pure Transport Decoupling**: Free of HTTP or provider-specific protocol dependencies. Operates over a clean asynchronous stream contract (`LLMClientTrait`). In production it is powered by `thunder-agent-providers`' `RpiAiClient`, which streams in-process (no child process, no Node dependency).
- **Checkpoint Context Compaction**: Between two compactions the request prefix stays byte-identical (full provider prompt-cache hits). Only when approaching the model's real window limit (`max_context_tokens - reserve_tokens`) is older history swapped for a single LLM-generated structured checkpoint (Goal / Progress / Key Decisions / Next Steps / Critical Context + read/modified file lists), keeping the last `keep_recent_tokens` verbatim. Summarization requests opt out of cache writes (`cache_retention: "none"`); when summarization is unavailable it degrades to an **emergency trim** (atomically dropping the oldest complete turns, never splitting a tool call from its result); the raw pre-compaction transcript is preserved via `AgentRunResult.raw_messages`.
- **One Tool-Execution Funnel**: every tool call (built-in / plugin / MCP / runtime remote) goes through `ToolExecutor::run_tool` — cancellation, scope, guard, admission, prompt, execute, trace. **There is no onion**: the order is written down in one place, and nothing sits between the decision and the execution.
- **Ultra-Low Latency & Footprint**: Framework scheduling overhead is only **~5.7 µs (0.0057 ms)** per task. Resident memory (RSS) under 10k concurrency is **< 10 MB**, with zero garbage collection pause.
- **Native Parallel Tool Dispatch**: Executes native functions and non-blocking asynchronous Bash subprocesses concurrently with timeout fuses and head/tail smart truncation.
- **Observable Event Streaming**: Full-lifecycle backpressured broadcast stream supporting `TurnStart`, `TokenDelta`, `ReasoningDelta`, `ToolExecResult`, and `LoopComplete`.
- **Admission Hooks & a Trace of Every Call**: the kernel offers exactly three named seams — `ToolScope` (the host answers "where does this land"), `ToolGate` (the host answers "may it run"), `ToolTraceSink` (the host takes the record). **No gate installed means everything but the guard runs.** Every call leaves exactly one trace, which records the decision itself (`guard` / `gate` / `answer` / `scope`), redacted before it reaches disk. Tools are **always registered in full**.

---

## 📊 Benchmark

Benchmarked on Apple Silicon (ARM64):

| Metric | Result | Note |
| :--- | :--- | :--- |
| **Token Estimation Throughput** | **1,465 MB/s** (1.53B chars/sec) | Zero-allocation, dual English/Chinese |
| **10k Concurrency Dispatch** | **174,838 loops / sec** | 10,000 concurrent loops completed in 57ms |
| **Single-Task Framework Overhead** | **5.72 µs (0.0057 ms)** | Clean scheduling latency excluding external network |
| **10k Concurrency Resident Memory (RSS)** | **8.19 MB** | Minimal footprint for microservices and edge embedding |

---

## 🛠️ Quickstart

### 1. Basic Loop & Tool Registration

```rust
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
// Tools live in the capability pack (`thunder-agent-pack-code`), not the kernel (see §4).
use thunder_agent_pack_code::builtin::{BashTool, ReadFileTool, WriteFileTool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize configuration (default 50-turn safety ceiling)
    let config = AgentConfig::new("gpt-4o")
        .with_system_prompt("You are a high-speed AI engineering assistant.")
        .with_max_turns(50); // or .with_unlimited_turns()

    let mut agent = AgentLoop::new(config);
    
    // 2. Register required tools
    agent.register_tool(Arc::new(BashTool::default()));
    agent.register_tool(Arc::new(ReadFileTool::default()));
    agent.register_tool(Arc::new(WriteFileTool::default()));

    // 3. Execute closed-loop task
    let result = agent.run("Check system git status and summarize the repository", None).await?;
    
    println!("Final Output: {:?}", result.final_content);
    println!("Total Turns: {}", result.stats.total_turns);
    println!("Tool Executions: {}", result.stats.total_tool_executions);

    Ok(())
}
```

---

### 2. Event Subscription & Real-Time Streaming

```rust
use thunder_agent_loop::prelude::*;

let mut agent = AgentLoop::new(config);
let mut event_rx = agent.subscribe_events();

tokio::spawn(async move {
    while let Ok(observed) = event_rx.recv().await {
        match observed.event {
            AgentEvent::TurnStart { turn, .. } => {
                println!("▶ [{}] Turn {} started", observed.agent_id, turn);
            }
            AgentEvent::TokenDelta { delta, .. } => {
                print!("{}", delta);
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                // Reasoning chain (DeepSeek-R1 / o1)
                print!("[Thinking] {}", delta);
            }
            AgentEvent::ToolExecResult { name, result, .. } => {
                println!("🛠️ [{}] Tool '{}' finished ({}ms)", observed.agent_id, name, result.duration_ms);
            }
            AgentEvent::TurnEnd { finish_reason, stats, .. } => {
                println!("⏹ Turn concluded ({}, {}ms)", finish_reason, stats.duration_ms);
            }
            _ => {}
        }
    }
});
```

---

### 3. Checkpoint Context Compaction Configuration

```rust
use thunder_agent_loop::prelude::*;

let mut config = AgentConfig::new("deepseek-chat");
config.pruning = ContextPruningConfig {
    // Total physical context window (e.g. 1,000,000 tokens)
    max_context_tokens: 1_000_000,
    // Trigger: compaction fires when estimated > max_context_tokens - reserve_tokens
    reserve_tokens: 16_384,
    // Newest tokens kept verbatim (never summarized)
    keep_recent_tokens: 20_000,
    // Optional dedicated smaller/faster summarizer model (defaults to the main model)
    summarizer_model: None,
    summarizer_max_tokens: 4096,
    pin_system_prompt: true,
};
```

---

### 4. Cross-Agent File Concurrency Lock

The lock is **not in the kernel**: `FILE_MUTATION_LOCKS` belongs to the write tool in `thunder-agent-pack-code/src/builtin/fs.rs` — an atomic write is part of what "write a file" *is*, not a layer an assembly could leave out. When multiple agent units or concurrent tool calls target the same file path, the write tool acquires it:

```rust
// Process-wide normalized path mutex table
// Concurrent writes to the SAME file are queued safely.
// Concurrent writes to DIFFERENT files proceed fully parallel.
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

// Write flow: acquire path lock → write temp file → atomic shadow-rename commit → release lock + best-effort table cleanup
let lock = FILE_MUTATION_LOCKS.lock().unwrap()
    .entry(normalized_path)
    .or_insert_with(|| Arc::new(TokioMutex::new(())))
    .clone();
let _guard = lock.lock().await;
```

**Scope & lifecycle**: the table is **process-local** — every agent, task, and tool inside one OS process (including all daemon `run_task`s) shares it; it does **not** span processes (a TUI running alongside the daemon gets no mutual exclusion). Each uncontended write releases its entry best-effort (skipped when waiters exist), so long-lived processes never grow the table unboundedly.

---

## 🔒 Admission & Cooperative Pausing

### Admission: `ToolGate` / `ToolScope`

The kernel carries **no approval opinion**. It offers two hooks and one guard that cannot be argued with:

```rust
// The host answers "where does this call land" — a classification, never a verdict.
pub trait ToolScope { fn scope_of(&self, tool: &str, effect: ToolEffect, args: &Value) -> Scope; }

// The host answers "may it run" — it may only tighten; the dialog is the kernel's.
pub trait ToolGate {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict;
    fn record_answer(&self, request: &ToolGateRequest<'_>, answer: &ApprovalAnswer) {}
    fn label(&self) -> &str { "gate" }
}
```

**With neither installed, everything but the guard runs** — that is the headless-host red line. The approval modes (`plan` / `ask` / `acceptEdits` / `yolo`) are implemented by `ModePlugin` in `thunder-agent-root`, and can be unloaded.

```rust
let agent = AgentLoop::new(cfg)
    .with_tool_scope(Arc::new(CodeScope::from_agent_config(&cfg, Some(&ws))))
    .with_tool_gate(Some(mode_plugin as Arc<dyn ToolGate>), Some(ui));
```

### `PauseGate` (Cooperative Pausing)

`CancellationToken` handles irreversible task termination; `PauseGate` handles non-destructive, resumable task pausing.

```rust
let handle = agent.start("Long running task", None)?;
handle.pause();          // Takes effect at next tool boundary
handle.resume();         // Resumes execution
```

The pause checkpoint is placed **before tool dispatch**. Active tools finish cleanly before pausing, preventing file corruption.

---

## 📂 Module Structure

```text
src/
├── core/                  # Context buffer, state tracker, token estimator, PauseGate
├── loop_engine/           # Core loop engine, event emitter, task state machine
├── pruning/               # Checkpoint context compaction (+ emergency trim floor)
├── stream/                # Pure LLMClientTrait contract (+ unconfigured placeholder)
├── tools/                 # Tool registry and parallel executor
│   ├── executor.rs        # run_tool: the one entry (guard -> gate -> ask -> execute -> trace)
│   ├── guard.rs           # Kernel guard: dangerous commands (token-based) + critical paths
│   └── trace.rs           # ToolTrace, redaction, ToolTraceSink
└── types/                 # Messages, events, tools, and config definitions
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
