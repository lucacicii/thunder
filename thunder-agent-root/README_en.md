# ⚡ Thunder Agent Root

> **Thunder Agent Microkernel Host Framework & Dynamic Plugin Assembler**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-root` is the microkernel host built on top of [`thunder-agent-loop`](../thunder-agent-loop). It dynamically composes the single-agent loop engine with multi-turn session persistence, external skill discovery, MCP remote tools, and model telemetry, exposing a unified host execution entry point.

---

## 🚀 Architectural Design

1. **Microkernel Architecture (Explicit Assembly)**:
   - The host contributes three lists — `PromptSection`, `ToolPack`, `Observer` — plus a per-run `Prepared` hook. There is no plugin registry and no keyword selection: registered is active.
     - **`ConversationPlugin`**: Multi-turn session tracking, Turn grouping, and atomic filesystem persistence.
     - **`MemoryPlugin`**: Layers `.thunder` long-term memory (`THUNDER.md` / `memory/*.md`) into the system prompt.
     - **`SkillsPlugin`**: Playbook directory discovery, prompt injection, and dynamic skill inspection tools.
     - **`McpPlugin`**: Model Context Protocol connections and remote tool bridging.
     - **`MetricsPlugin`**: Model telemetry — live/average tok/s, token breakdown, prompt-cache hit rate, context occupancy, and durations; streamed as `metrics_update` custom events without touching the kernel.
     - **`StatusPlugin`**: Execution status — whether the agent is thinking, streaming, which tool it is running, what it is waiting on, whether it is paused, and when it finished; streamed as `agent_status` custom events **only on a transition**. The single authority for "what is it doing right now".
2. **Registered is Active (No Hidden Routing)**:
   - No selector, no secondary LLM intent classification: whatever the host assembled is what this run has.
   - `plugins.disabled` in `.thunder/config.json` is the only subtraction, so the tool table and the prompt prefix cannot drift between turns — the condition for a stable provider-side prompt cache.
3. **Global Skills Scan Cache (120s TTL)**:
   - Features `DEFAULT_SKILLS_CACHE` with a 120-second TTL over skill directories, eliminating redundant disk walks and filesystem bottlenecks during multi-turn interactions.
4. **One-line Standard Assembly (`StandardHostBuilder`)**:
   - The canonical assembly entry point: mounts conversation, telemetry, execution status, memory, web, skills and MCP by default; host-specific capabilities (e.g. the daemon's `ask_user`) are appended after `build` via `ThunderRoot::with_pack(ToolPack::new("ask_user").with_tool(...))`.
5. **Declarative Plugin Configuration & Discovery (`PluginLoader`)**:
   - Declare the `plugins` section in `.thunder/config.json` and its layered overrides:
     - `builtin`: Whitelist enabled built-in tools (e.g. `["read_file", "write_file", "grep"]`);
     - `disabled`: Blacklist unwanted tools or interceptors (e.g. `["bash"]`), with bidirectional alias support (`read` <=> `read_file`);
     - `mcpServers`: Declare external MCP server processes, automatically spawned, discovered, and bridged as standard `ToolPlugin` instances.

---

## 🛠️ Quickstart

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::MemoryConversationStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_cfg = AgentConfig::new("deepseek-chat");

    // Standard plugin suite, assembled through StandardHostBuilder
    let root = StandardHostBuilder::new(Arc::new(MemoryConversationStore::new()))
        .build(ThunderRoot::new(base_cfg));

    // Run task
    let options = RootRunOptions::default();
    let result = root.run("Check git status and summarize recent commits", options).await?;

    // What is installed is readable before the run (there is no "selection")
    println!("Components: {:?}", root.component_ids());
    println!("Final Content: {:?}", result.final_content);
    println!("Total Turns: {}", result.run_result.stats.total_turns);
    println!("Live tok/s: {:?}", result.metrics.map(|m| m.live_tps));

    Ok(())
}
```

---

## 🔌 Standard Plugin Roster

| Plugin ID | Name | Activation Strategy | Core Capabilities |
| :--- | :--- | :--- | :--- |
| **`conversation`** | Session Persistence | **Permanent Baseline** | Tracks multi-turn dialogue, updates `index.json`, atomic file writes. One engine turn lands as **one** assistant message (its text, all of the turn's `tool_calls`, its chain-of-thought `reasoning`, its `stats`), followed by that turn's tool results — the reasoning has nowhere else to live: the provider streams it once and the trace counts it as noise |
| **`metrics`** | Model Telemetry | **Permanent Baseline** | Tracks live/average tok/s (TTFT and tool time excluded), token breakdown, cache hit rate, context occupancy and durations; emits `metrics_update` snapshots |
| **`agent_status`** | Execution Status | **Permanent Baseline** | One authoritative snapshot per session: phase (`idle`/`thinking`/`streaming`/`tool`/`awaiting_user`/`paused`/`compacting`/`done`/`error`) + current tool + turn. `agent_status` custom events are emitted **only on a transition** (`seq` is monotonic); `awaiting_user`/`paused` are reported by the host through `note_awaiting`/`note_paused` |
| **`skills`** | Skills Registry | **Permanent Baseline** | Scans `~/.agents/skills`, exposes `load_skill` / `list_skills` |
| **`mcp`** | MCP Client | Dynamic Keyword Triggers | Connects to external MCP servers and bridges remote tools |
| **`memory`** | Long-Term Memory | **Permanent Baseline** | Layered load of `.thunder`'s `THUNDER.md` / `memory/*.md`, appended to the system prompt |
| **`mode`** | Approval-Mode Gate | **Explicitly installed by the host** | Implements `ToolGate`: `plan` / `ask` / `acceptEdits` / `yolo`, and carries `write_plan` / `exit_plan_mode`; under `plan` a shell command proved read-only runs unprompted (the verdict lives in the kernel's `read_only.rs`); a mode change is announced through `set_mode_announcing` (a no-op is not announced); `with_persisted_rules` writes "always allow" rules to `<data root>/permissions.json` |
| **`web`** | Web Tools | **Permanent Baseline** | `web_fetch`: fetches a URL, converts the page to markdown, and answers a prompt with the utility model (markdown fallback when none is configured). `web_search`: returns titles, URLs, and summaries. The backend is chosen by `web.search.provider` / `web.fetch.provider`: by default the keyless engines (DuckDuckGo first, Exa as a fallback) and the local fetcher; under `auto`, DeepSeek's `/v1/tools/search` and `/v1/tools/fetch` are preferred/filled in (billed per request). DeepSeek searches also send the user's original request as `ori_question` to sharpen intent |


## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
