# ⚡ Thunder Agent Root

> **Thunder Agent Microkernel Host Framework & Dynamic Plugin Assembler**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-root` is the microkernel host built on top of [`thunder-agent-loop`](../thunder-agent-loop). It dynamically composes the single-agent loop engine with multi-turn session persistence, external skill discovery, MCP remote tools, TypeScript script plugins, and multi-agent orchestration pipelines, exposing a unified host execution entry point.

---

## 🚀 Architectural Design

1. **Microkernel Architecture (Everything is a Plugin)**:
   - The host maintains only minimal execution scheduling; all capabilities are implemented through the `ThunderPlugin` contract:
     - **`ConversationPlugin`**: Multi-turn session tracking, Turn grouping, and atomic filesystem persistence.
     - **`SkillsPlugin`**: Playbook directory discovery, prompt injection, and dynamic skill inspection tools.
     - **`McpPlugin`**: Model Context Protocol connections and remote tool bridging.
     - **`ScriptPlugin`**: Native TypeScript single-file plugin execution with blue-green hot reload.
     - **`OrchestraPlugin`**: Multi-agent scheduling and delegated subagent drill-down.
2. **Deterministic Zero-LLM Latency Activation**:
   - Replaces sluggish secondary LLM intent classification (which added 1~2s latency per turn).
   - `PluginSelector` enforces a **deterministic baseline**: conversation and skills plugins are permanently active for all runs;
   - Complex plugins (orchestra, MCP, etc.) are activated selectively via keyword intent triggers, ensuring **zero added latency, minimal token consumption, and maximal extensibility**.
3. **Global Skills Scan Cache (120s TTL)**:
   - Features `DEFAULT_SKILLS_CACHE` with a 120-second TTL over skill directories, eliminating redundant disk walks and filesystem bottlenecks during multi-turn interactions.
4. **Convenient Standard Configuration (`with_standard_plugins`)**:
   - A one-line builder that mounts the standard skills and MCP plugins out of the box; conversation and orchestra plugins can be appended as needed.

---

## 🛠️ Quickstart

```rust
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_cfg = AgentConfig::new("deepseek-chat");

    // Assemble standard plugin suite (skills, mcp); append more plugins as needed
    let mut root = ThunderRoot::new(base_cfg)
        .with_standard_plugins()
        .with_plugin(ConversationPlugin::with_memory_store());

    // Run task
    let options = RootRunOptions::default();
    let result = root.run("Check git status and summarize recent commits", options).await?;

    println!("Active Plugins: {:?}", result.selection.active_plugin_ids);
    println!("Final Content: {:?}", result.final_content);
    println!("Total Turns: {}", result.run_result.stats.total_turns);

    Ok(())
}
```

---

## 🔌 Standard Plugin Roster

| Plugin ID | Name | Activation Strategy | Core Capabilities |
| :--- | :--- | :--- | :--- |
| **`conversation`** | Session Persistence | **Permanent Baseline** | Tracks multi-turn dialogue, updates `index.json`, atomic file writes |
| **`skills`** | Skills Registry | **Permanent Baseline** | Scans `~/.agents/skills`, exposes `load_skill` / `list_skills` |
| **`mcp`** | MCP Client | Dynamic Keyword Triggers | Connects to external MCP servers and bridges remote tools |
| **`script_plugin`** | TS Script Engine | Dynamic Keyword Triggers | Native TypeScript plugin runner with hot-reload and error immunity |
| **`orchestra`** | Multi-Agent Orchestrator | Dynamic Keyword Triggers | Exposes `delegate_subtask` tool for isolated subagent execution |

### Client injection for `delegate_subtask` sub-agents (critical)

`DelegateTool` builds a **fresh, independent** `AgentLoop` from its internal factory on every invocation — it neither goes through `Scheduler::spawn_unit` nor **implicitly inherits** the outer agent's LLM client. The plugin therefore forwards the `client_factory` from its `OrchestraConfig` into every delegated sub-agent (the same composition-root seam as `spawn_unit`):

- **Real mode**: attach `OrchestraConfig::with_client_factory(...)` (the TUI constructs and injects it automatically from `ProviderRegistry`; `examples/cli.rs` wires the same source).
- **Without a factory it fails honestly**: the sub-agent's first LLM call surfaces as a tool error naming the `OrchestraConfig::with_client_factory` fix, instead of a vague "finished with no final content".
- **Without a base config the tool is not registered**: no phantom `gpt-4o` sub-agent — a warning is logged and registration is skipped (attach `OrchestraConfig::with_base`).

The cross-crate seam is locked by `tests/orchestra_plugin_test.rs` (invokes `delegate_subtask` through `OrchestraPlugin::tools()` and asserts the factory client is used).

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
