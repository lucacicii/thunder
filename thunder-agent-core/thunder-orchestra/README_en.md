# Thunder Orchestra

> **Multi-Agent Orchestration Scheduler (Scheduler B)**

[English](README_en.md) | [简体中文](README.md)

Thunder Orchestra is the high-level scheduler responsible for composing multiple [`thunder-agent-loop`](../../thunder-agent-loop) units.

Under Thunder's **A/B Architectural Contract**:
- **Agent A (`thunder-agent-loop`)**: A complete single-agent unit driving autonomous multi-turn reasoning, tool execution, and context management.
- **Agent B (`thunder-orchestra`)**: The high-level scheduler responsible for creating, starting, monitoring, merging, and cancelling multiple A units. **B never drives A's internal turns**.

```text
B (thunder-orchestra)  ──depends on──►  A (thunder-agent-loop)
A  never imports B
```

---

## Directory Layout

```text
thunder/
├── thunder-agent-loop/                               # A (Single-Agent Loop Unit)
└── thunder-agent-core/
    └── thunder-orchestra/                            # B (Multi-Agent Scheduler)
        ├── src/
        │   ├── config.rs                             # OrchestraConfig, Topology, UnitSpec
        │   ├── decomposer.rs                         # TaskDecomposer, SubTask decomposition
        │   ├── delegate.rs                           # DelegateTool (A-calls-A tool bridge)
        │   ├── router.rs                             # IntentRouter deterministic routing
        │   ├── scheduler.rs                          # Scheduler (Sequential / Parallel / FanOut)
        │   ├── store.rs                              # RunStore run trace persistence
        │   └── synthesizer.rs                        # Synthesizer multi-agent report aggregation
        └── tests/
```

---

## What B Does and Does Not Do

| B Owns | B Never Touches |
|---|---|
| Creating and managing N `AgentLoop` instances with distinct `agent_id`s | Driving A's internal turn progression |
| Calling `start` / `join` / `cancel` on units | Session UI, Electron, or FFI rendering |
| Demultiplexing `ObservedEvent` streams by `agent_id` | Forcing global agent graphs into A's engine |
| Persisting `AgentRunResult` artifacts to disk | Violating A's loop / prune / tools encapsulation |
| Providing optional `DelegateTool` (subagent delegation) | Maintaining implicit shared blackboards inside A |

---

## Topologies & Execution Modes

| Topology | Purpose | Role & Division Mechanism | Best For |
|---|---|---|---|
| **`Single`** | Single-agent autonomous execution | Single versatile agent reasoning and executing independently | Standard queries, atomic commands, isolated file edits |
| **`Sequential`** | Chained sequential pipeline | Previous outputs injected as next stage's input (Planner ➔ Coder ➔ Reviewer) | Multi-phase feature development & iterative refinement |
| **`Parallel`** | Multi-perspective council review | **Role context injection**: Each unit receives dedicated system prompts and responsibility boundaries | Multi-dimensional audits, security and performance evaluations |
| **`FanOut`** | **True subtask decomposition** | **Structured task decomposition**: `TaskDecomposer` splits complex prompts into distinct subtasks executed concurrently | Batch operations, modular independent analyses |

### Synthesizer & File Concurrency Protection

- **Result Synthesizer (`Synthesizer`)**: After `Parallel` or `FanOut` execution completes, an optional synthesis node aggregates individual outputs into a unified resolution report (`synthesis`), sparing users from sifting through disparate answers.
- **Cross-Agent File Concurrency Lock**: Protected by `TransactionMiddleware`'s process-wide normalized path mutex (`FILE_MUTATION_LOCKS`). Concurrent file writes across units or tools are serialized safely, preventing silent overwrites.

---

## Testing & Execution

The default verification contract includes:
- **Pipeline Handoff**: `planner` ➔ `coder`. The later unit must receive the earlier unit's `final_content`.
- **Parallel Review & Role Injection**: Each unit receives dedicated `role` prompt instructions.
- **FanOut Decomposition & Concurrent Execution**: Complex tasks are split by numbered lists or roles and dispatched concurrently.
- **Synthesizer Report Aggregation**: Final structured consensus and summary generation.

Results are persisted under `./runs/<run_id>/<agent_id>.json`.

### Commands

```bash
cd thunder-agent-core/thunder-orchestra

# Run unit and integration tests
cargo test --test scheduler_test
cargo test --test router_test

# Run sequential pipeline mock verification
cargo run -- --mock pipeline "add a health check"

# Run parallel review mock
cargo run -- --mock parallel "review the security and performance of src/lib.rs"

# Run fan-out subtask decomposition mock
cargo run -- --mock fanout "1. Refactor networking 2. Optimize parser 3. Update tests"

# Run orchestrator health check (writable stores and reachable LLM)
cargo run -- health --mock
```

### Health Check Output

```json
{
  "status": "ok",
  "checks": [
    { "name": "store_writable",   "passed": true, "detail": "..." },
    { "name": "scratch_writable", "passed": true, "detail": "..." },
    { "name": "llm_reachable",    "passed": true, "detail": "mock client streamed a Completed chunk" }
  ]
}
```

---

## Dependency Specification

```toml
[dependencies]
thunder-agent-loop = { path = "../../thunder-agent-loop" }
```
