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
| **`FanOut`** | **Subtask fan-out** | **Heuristic decomposition**: `HeuristicDecomposer` detects numbered/bulleted list slices and fans them out concurrently; natural-language prompts fall back to per-role perspective slices | Batch list tasks, modular independent analyses |

### Synthesizer & File Concurrency Protection

- **Result Synthesizer (`Synthesizer`)**: After `Parallel` or `FanOut` execution completes, an optional synthesis node aggregates individual outputs into a unified resolution report (`synthesis`), sparing users from sifting through disparate answers. **Honest degradation**: LLM synthesis runs only in real mode with a configured synthesizer unit **and** a resolved client; otherwise unit outputs are aggregated verbatim with the reason stated — a fake "successfully synthesized" verdict is never fabricated.
- **Cross-Agent File Concurrency Lock**: Protected by `TransactionMiddleware`'s **process-local** normalized path mutex (`FILE_MUTATION_LOCKS`). Concurrent writes to the same file are serialized while different files proceed in parallel; entries are released best-effort after uncontended writes so long-lived processes never grow the table unboundedly. Note: the lock does NOT span processes (a TUI running alongside the daemon gets no mutual exclusion).

### Real-Mode Client Injection (Critical)

B is a pure scheduler and **never resolves transport itself**. Real mode (`use_mock = false`) requires the composition root (TUI / daemon / CLI) to inject a `ClientFactory`:

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_orchestra::{ClientFactory, OrchestraConfig, Topology, UnitSpec};

let factory: ClientFactory = Arc::new(|cfg: &AgentConfig| {
    my_registry
        .resolve(&cfg.model)
        .and_then(|spec| my_client_for(spec, cfg.request_timeout_ms).ok())
});

let orchestra = OrchestraConfig::new(Topology::Parallel)
    .with_client_factory(factory)          // required for real mode
    .with_unit(UnitSpec::new("planner", "planner", base.clone()));
```

**Fail-fast semantics**:
- Real mode without a factory → `dispatch` errors before any unit starts (instead of every unit dying mid-run on `UnconfiguredLLMClient`).
- Factory returning `None` for a unit → error naming the unit and its model.
- Synthesizer without a resolvable client → honest verbatim aggregation (see above), never a fake report.
- The `health` real-mode probe goes through the same factory.

The TUI builds this factory internally from its `ProviderRegistry` (same `client_for` path as `ThunderRoot`) and injects distinct persona system prompts for planner/coder/reviewer/synthesizer; the CLI resolves from the default registry when `--mock` is absent and exits with code `2` when unconfigured.

---

## Testing & Execution

The default verification contract includes:
- **Pipeline Handoff**: `planner` ➔ `coder`. The later unit must receive the earlier unit's `final_content`.
- **Parallel Review & Role Injection**: Each unit receives dedicated `role` prompt instructions.
- **FanOut Decomposition & Concurrent Execution**: Structured list tasks are split into slices; natural-language prompts fall back to per-role perspectives, all dispatched concurrently.
- **Synthesizer Report Aggregation**: Final structured consensus and summary generation (LLM synthesis in real mode; honest verbatim aggregation otherwise).
- **Real-Mode Client Injection**: `real_mode_test.rs` proves units and the synthesizer stream through the factory-provided client, and that missing factories fail fast.

Results are persisted under `./runs/<run_id>/<agent_id>.json`.

### Commands

```bash
cd thunder-agent-core/thunder-orchestra

# Run unit and integration tests
cargo test --test scheduler_test
cargo test --test router_test
# Real-mode (non-mock branch) client injection tests
cargo test --test real_mode_test

# Run sequential pipeline mock verification
cargo run -- --mock pipeline "add a health check"

# Run parallel review mock
cargo run -- --mock parallel "review the security and performance of src/lib.rs"

# Run fan-out subtask decomposition mock
cargo run -- --mock fanout "1. Refactor networking 2. Optimize parser 3. Update tests"

# Real mode (reads ~/.thunder/models.json + auth.json; exit code 2 when unconfigured)
MODEL=deepseek-chat cargo run -- parallel "review src/lib.rs"

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
# Used ONLY by the `thunder-orchestra` binary (composition root) to build the
# real-mode factory; the library itself stays transport-free.
thunder-agent-providers = { path = "../../thunder-agent-providers" }
```
