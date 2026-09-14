# Thunder Orchestra

Scheduler **B** for composing many [`thunder-agent-loop`](../../thunder-agent-loop) units (part of the `thunder-agent` monorepo).

[English](README_en.md) | [简体中文](README.md)

A (`thunder-agent-loop`) is a complete single-agent unit. This crate is B: it
creates, starts, joins, and cancels many A's. It never drives A's turns.

```
B  ──depends on──►  A (thunder-agent-loop)
A  never imports B
```

## Layout

```
~/Documents/GitHub/thunder/thunder-agent-loop                # A (Single Agent Unit)
~/Documents/GitHub/thunder/thunder-agent/                   # thunder-agent monorepo
└── thunder-orchestra/                                      # B (Scheduler sub-package)
```

## What B does

| B | Not B |
|---|---|
| Create N `AgentLoop`s with distinct ids | Drive A's inner turns |
| `start` / `join` / `cancel` per unit | Session UI, Electron, FFI |
| Demux `ObservedEvent` by `agent_id` | Agent graph inside A |
| Persist `AgentRunResult.messages` | A's loop, prune, tools |
| Optional `DelegateTool` (A calling A) | Shared blackboard in A |

## Test

B has no A-style example runner. Use `./test.sh`.

Default contract is **pipeline handoff**: `planner` → `coder`. B only `start` / `join`. The later unit must see the earlier unit's `final_content`. Results land in `./runs/<run_id>/<agent_id>.json`.

```bash
cd ~/Documents/GitHub/thunder/thunder-agent/thunder-orchestra

# Default: cargo test + mock pipeline + handoff asserts
./test.sh

# Scheduler unit tests only
./test.sh test

# Sequential planner → coder (default prompt: "add a health check")
./test.sh pipeline
./test.sh run "add a health check"

# Parallel planner + reviewer (persist only, no handoff assert)
./test.sh parallel
```

Mock (no `OPENAI_API_KEY`) checks:

- `planner.json` and `coder.json` share one `run_id`
- both units finish `done` with `total_turns == 2`
- `coder.final_content` contains `planner.final_content`
- coder's answer is not the raw brief

```bash
# Live LLM (same env as A; current process must export the key)
export OPENAI_API_KEY=...
export OPENAI_API_BASE=https://api.openai.com/v1
export MODEL=gpt-4o
./test.sh pipeline "add a health check"
```

Live mode skips mock-specific turn / text checks. It still requires both units to persist and produce `final_content`.

Direct cargo, if you do not want the script:

```bash
cargo test --test scheduler_test
cargo run -- --mock pipeline "add a health check"
cargo run -- parallel "review the repo"
```

### Health check

Before launching a real pipeline, verify the orchestrator with a cheap, scriptable
self-check: writable store + scratch dirs, and a reachable (or mocked) LLM.

```bash
# Standalone: prints a pretty JSON HealthReport, exit 0 for ok/degraded, 1 for failed
cargo run -- health --mock

# Or via the runner:
./test.sh health
```

Report shape (pretty JSON):

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

`status` ∈ `ok | degraded | failed`. The `health` command is orthogonal to the
topology and never drives a unit's turns.

## Depend on A

```toml
thunder-agent-loop = { path = "../../thunder-agent-loop" }
# later:
# thunder-agent-loop = { git = "https://github.com/lucacicii/thunder.git", tag = "v0.1.0" }
```
