# Architecture: A is a unit, B is a scheduler

This crate (`thunder-agent-loop`) is **Agent A**: a complete, independently runnable
single-agent unit. Another Rust application (**Agent B**) may depend on A and
orchestrate many A instances. A never depends on B.

```
A  = one AgentLoop, one task at a time, full inner turn loop
B  = optional scheduler / multi-agent graph (another crate)
```

## Layers

| | A (this crate) | B (downstream app) |
|---|---|---|
| Role | A complete single Agent | Scheduler / multi-agent graph |
| Lifecycle | create → register tools → `start`/`run` → events → join/cancel | Creates N agents, routes work, retries, persists product state |
| Dialogue | Multi-turn **inside one task** (the loop's job) | Cross-agent / cross-task routing and storage |
| Standalone | Required. `examples/cli` is one A | Optional. A works with no B |
| Composition | B calls `AgentLoop::start` / `join` / `cancel` | B must not become A's inner loop |

A owns the closed `run()` / `start()` loop: it streams the LLM, executes tools,
prunes context, trips loop guards, and finishes with `LoopComplete`.
That is unit completeness, not "stealing B's job".

B must **not** drive A's turns. Do not split A into an external stepper
(`step` + `execute_tools` as B's main API). `step` is not the integration surface.

B may persist `AgentRunResult.messages` after `join()`. That is B's product
storage, not a missing session layer in A.

## Unit contract

```rust
let mut agent = AgentLoop::new(config).with_id("researcher");
agent.register_tool(Arc::new(BashTool::default()));

let handle = agent.start("Investigate the repo", None)?;
// handle.agent_id() == "researcher"
// handle.events() is a reliable per-run stream (backpressure, no silent drop)
// handle.status() is live
// handle.cancel() stops this task only

let result = handle.join().await?;
```

`run()` is `start` + drain events + `join`. CLI and tests use `run()`.
Schedulers use `start` so they can run many agents side by side.

Rules:

1. **One in-flight task per `AgentLoop`.** A second `start`/`run` while busy
   returns `AgentError::AlreadyRunning`. Parallelism = `new` another instance.
2. Events are tagged with `agent_id` so a process with many A's can demux.
3. Scratchpad files are isolated by `agent_id` under the configured `base_dir`.
   A does not delete them unless `auto_cleanup` is explicitly set.
4. A does not install a global `tracing` subscriber. Hosts (CLI or B) do.
5. A does not know about B: no agent graph, no router, no shared blackboard.
6. Nested multi-agent (A calling A) is a tool in B (`DelegateTool` wrapping
   another `AgentLoop`). Do not grow a runtime graph inside A.

## What stays in A

- Closed autonomous loop (`run` / `start`)
- Streaming OpenAI-compatible client + `LLMClientTrait`
- Tool registry, parallel execution, timeout, UTF-8 truncation
- In-memory context pruning
- Loop-level repetition / error circuit breaker
- Cancellation via `CancellationToken`
- Optional built-in tools as **parts** (`BashTool`, file tools) — not auto-registered
- CLI example that runs one agent to completion

## What never enters A

- Session store, conversation list, multi-window UI
- Electron / napi / FFI (separate binding crate if ever needed)
- Multi-agent topology, routing table, shared memory between agents
- Host tracing subscriber, host tokio runtime ownership
- Product policy: workspace sandbox, command approval, API key storage

## Dependency direction

```
B  ──depends on──►  A (this crate)
A  never imports B
```

B pins A by git tag or crates.io version. A's CI does not build B.
