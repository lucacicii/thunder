# ⚡ Thunder Agent Root

> **Thunder Agent Microkernel Host Framework & Dynamic Plugin Assembler**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-root` is the microkernel host built on top of [`thunder-agent-loop`](../thunder-agent-loop). It dynamically composes the single-agent loop engine with multi-turn session persistence, external skill discovery, MCP remote tools, and model telemetry, exposing a unified host execution entry point.

---

## 🚀 Architectural Design

1. **Microkernel Architecture (Everything is a Plugin)**:
   - The host maintains only minimal execution scheduling; all capabilities are implemented through the `ThunderPlugin` contract:
     - **`ConversationPlugin`**: Multi-turn session tracking, Turn grouping, and atomic filesystem persistence.
     - **`MemoryPlugin`**: Layers `.thunder` long-term memory (`THUNDER.md` / `memory/*.md`) into the system prompt.
     - **`SkillsPlugin`**: Playbook directory discovery, prompt injection, and dynamic skill inspection tools.
     - **`McpPlugin`**: Model Context Protocol connections and remote tool bridging.
     - **`MetricsPlugin`**: Model telemetry — live/average tok/s, token breakdown, prompt-cache hit rate, context occupancy, and durations; streamed as `metrics_update` custom events without touching the kernel.
2. **Deterministic Zero-LLM Latency Activation**:
   - Replaces sluggish secondary LLM intent classification (which added 1~2s latency per turn).
   - `PluginSelector` enforces a **deterministic baseline**: conversation, skills and telemetry plugins are permanently active for all runs;
   - Complex plugins (MCP, etc.) are activated selectively via keyword intent triggers, ensuring **zero added latency, minimal token consumption, and maximal extensibility**.
3. **Global Skills Scan Cache (120s TTL)**:
   - Features `DEFAULT_SKILLS_CACHE` with a 120-second TTL over skill directories, eliminating redundant disk walks and filesystem bottlenecks during multi-turn interactions.
4. **One-line Standard Assembly (`StandardHostBuilder`)**:
   - The canonical assembly entry point: mounts conversation, telemetry, memory, web, skills and MCP by default; host-specific capabilities (e.g. the daemon's `ask_user`) are appended after `build` via `ThunderRoot::with_plugin`.

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

    println!("Active Plugins: {:?}", result.selection.active_plugin_ids);
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
| **`conversation`** | Session Persistence | **Permanent Baseline** | Tracks multi-turn dialogue, updates `index.json`, atomic file writes |
| **`metrics`** | Model Telemetry | **Permanent Baseline** | Tracks live/average tok/s (TTFT and tool time excluded), token breakdown, cache hit rate, context occupancy and durations; emits `metrics_update` snapshots |
| **`skills`** | Skills Registry | **Permanent Baseline** | Scans `~/.agents/skills`, exposes `load_skill` / `list_skills` |
| **`mcp`** | MCP Client | Dynamic Keyword Triggers | Connects to external MCP servers and bridges remote tools |
| **`memory`** | Long-Term Memory | Dynamic Keyword Triggers | Layered load of `.thunder`'s `THUNDER.md` / `memory/*.md`, appended to the system prompt |
| **`mode`** | Approval-Mode Gate | **Explicitly installed by the host** | Implements `ToolGate`: `plan` / `ask` / `acceptEdits` / `yolo`, and carries `write_plan` / `exit_plan_mode` |
| **`web`** | Web Tools | **Permanent Baseline** | `web_fetch`: fetches a URL, converts the page to markdown, and answers a prompt with the utility model (markdown fallback when none is configured). `web_search`: queries keyless engines (DuckDuckGo first, Exa as a fallback) and returns titles, URLs, and snippets |


## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
