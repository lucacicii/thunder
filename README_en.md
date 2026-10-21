# Thunder

> **A very small Rust agent kernel with explicitly assembled hosts**

[English](README_en.md) | [简体中文](README.md)

Thunder is a coding agent written in Rust: a kernel that knows exactly two things (a `Tools` seam for running tools, an `LLMClientTrait` for getting a stream), and hosts that keep every product opinion to themselves. The workspace is a single Cargo workspace of 12 crates.

- Architecture and module map: [`docs/architecture.md`](docs/architecture.md)
- Permissions, approvals, the four modes: [`docs/permission-structure.md`](docs/permission-structure.md)
- Migrating 0.5 → 0.6: [`docs/migration-0.6.md`](docs/migration-0.6.md)
- Workspace and development conventions: [`WORKSPACE.md`](WORKSPACE.md)

---

## Design

1. **The kernel knows seams only** (`thunder-agent-loop`)
   - Tools arrive through `Tools::specs()` / `Tools::call()`; the model through `LLMClientTrait`. The kernel knows no HTTP dialect, no MCP, no approval.
   - Cancellation, timeouts and the pairing invariant (every announced call gets a result) are the kernel's guarantees.

2. **Hosts assemble explicitly** (`thunder-agent-root`)
   - A host contributes four lists to one run: `PromptSection` (prompt text), `ToolPack` (tools), `Observer` (events), `Prepared` (per-run setup) — all receiving the same `RunEnv`.
   - **Registered is active**: no selector, no keyword routing. `plugins.disabled` in `.thunder/config.json` is the only subtraction, so the tool table and prompt prefix cannot drift between turns.

3. **One tool funnel, one trace per call** (`thunder-agent-policy`)
   - Every call takes one path: cancel → scope → guard → mode gate → dialog → interceptors → registry → trace.
   - The guard is a floor no mode can loosen; with no UI attached a dialog counts as a refusal (fail-closed). See `docs/permission-structure.md`.

4. **Byte-bounded event stream** (`max_event_bytes`, 8 MiB by default)
   - The queue is bounded by bytes, not frame count; overflow is dropped and counted, so a host can read `EventDropCounters` and know its record may be incomplete.

5. **Checkpoint compaction with a concurrency cap**
   - Compaction runs only near the real window; a failed summary falls back to emergency trim; `max_concurrent_summaries` (default 2) caps in-flight summaries across agents sharing a limiter.
   - Between compactions the request prefix is byte-stable, which is what keeps the provider-side prompt cache warm.

6. **In-process model transport**
   - `thunder-agent-providers` streams HTTP directly: no child process, no Node. OpenAI chat / OpenAI Responses / Anthropic Messages and friends; unknown dialects fail loudly.

7. **Optional capability pack**
   - `thunder-agent-pack-code` provides `read_file` / `write_file` / `ls` / `find` / `grep` / `bash`; the kernel does not depend on it.
   - The program behind `bash` is a host decision (`ShellConfig`: `$SHELL` detection or pinned), and on Unix it runs in its own process group so a timeout kills the whole tree.

8. **Multiple hosts**
   - Terminal (`thunder-tui`), NDJSON sidecar (`thunder-agent-daemon`) and headless JSONL (`thunder-agent-runtime`) share the same assembly.

---

## Workspace (12 crates)

| Crate | Role |
|---|---|
| [`thunder-agent-types`](thunder-agent-types) | Pure wire: messages / events / tool definitions and results / policy declarations / scope / trace records |
| [`thunder-agent-loop`](thunder-agent-loop) | Kernel: loop, context, pruning, stream, cache, `seam` |
| [`thunder-agent-policy`](thunder-agent-policy) | Policy layer: tool abstractions, execution funnel, gate, UI, interceptors, read-only proof |
| [`thunder-agent-pack-code`](thunder-agent-pack-code) | Reference pack: fs / shell / search + `CodeScope` |
| [`thunder-agent-providers`](thunder-agent-providers) | Model catalog and in-process streaming transport |
| [`thunder-agent-skills`](thunder-agent-skills) | Skill parsing, registry and catalog injection |
| [`thunder-agent-mcp`](thunder-agent-mcp) | MCP client / transports / tool bridge (with a frame bound) |
| [`thunder-agent-root`](thunder-agent-root) | Host assembly (`Assembly` / `StandardHostBuilder`) |
| [`thunder-agent-daemon`](thunder-agent-daemon) | NDJSON sidecar |
| [`thunder-agent-runtime`](thunder-agent-runtime) | JSONL headless runtime (the host executes tools) |
| [`thunder-conversation`](thunder-agent-core/conversation) | Conversation stores (memory / filesystem) |
| [`thunder-tui`](thunder-agent-core/tui) | Ratatui terminal UI |

---

## Quick start

Rust 1.80+, no Node.js required.

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder

./run.sh      # interactive TUI
./daemon.sh   # NDJSON sidecar
./test.sh     # workspace test suite
```

Embedding your own host:

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::MemoryConversationStore;

let root = StandardHostBuilder::new(Arc::new(MemoryConversationStore::new()))
    .build(ThunderRoot::new(AgentConfig::new("deepseek/deepseek-chat"))
        .with_workspace(std::env::current_dir()?));

let mut handle = root
    .execute("run the tests and fix the failing one", RootRunOptions::default())
    .await?;
if let Some(mut events) = handle.take_events() {
    while let Some(event) = events.recv().await { /* forward to your UI */ }
}
let result = handle.join().await?;
println!("{:?}", result.final_content);
```

Host-specific capabilities are appended after `build`:

```rust
root = root.with_pack(ToolPack::new("my_tool").with_tool(Arc::new(MyTool)));
```

---

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features thunder-agent-daemon/testing-mock -- -D warnings
cargo test --workspace
```

CI runs the same set; the test map is in `docs/architecture.md` §14.4.

---

## License

[Apache License 2.0](LICENSE)
