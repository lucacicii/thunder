# Thunder

> **Ultra-lightweight, High-Performance, Modular Rust AI Agent Workspace & Runtime**

[English](README_en.md) | [简体中文](README.md)

Thunder is a modern AI Agent ecosystem built in Rust. Managed under a single Git monorepo and a unified Cargo workspace, it provides an end-to-end architecture spanning ultra-fast single-agent execution units, unified LLM transport, dynamic microkernel plugin composition, and multi-agent coordination (sequential pipelines, role-injected parallel reviews, task decomposition fan-out, and synthesis aggregation).

Hosted at [lucacicii/thunder](https://github.com/lucacicii/thunder). For monorepo layout, architecture dependencies, and development conventions, see **[WORKSPACE.md](WORKSPACE.md)** (or **[WORKSPACE_en.md](WORKSPACE_en.md)**).

---

## ⚡ Core Architectural Pillars

1. **A/B Architecture Contract (Unit vs Scheduler)**:
   - **Agent A (`thunder-agent-loop`)**: Atomic closed execution unit for a single task. Responsible for driving multi-turn reasoning, streaming token parsing, concurrent tool execution, and context pruning. Never couples multi-agent graph or shared blackboard logic.
   - **Agent B (`thunder-orchestra`)**: High-level scheduler that coordinates N independent A units. Supports sequential pipelines (`Sequential`), role-bound council reviews (`Parallel`), and structured subtask partitioning (`FanOut`), unified by a `Synthesizer` node into a consolidated report.
2. **`pi-bridge` Single Transport Convergence**:
   - Replaced high-maintenance custom Rust model dialect adapters with a single Node.js sidecar running `@earendil-works/pi-ai`, pre-bundled via esbuild.
   - Zero `npm install` required on fresh checkouts. Seamlessly unifies dialect handling, token accounting, and thinking level mappings across OpenAI, Anthropic Claude, DeepSeek, Google Gemini, Ollama, and more.
3. **Decoupled Tool Eviction & Large Context Preservation**:
   - Decoupled **tool output eviction** (`tool_eviction_threshold_tokens: 20_000`) from the model's total physical context window limit.
   - Trims noisy older tool execution logs during tool-heavy workloads while preserving human-assistant dialogue history up to the model's full context capacity (e.g. DeepSeek 1M tokens).
4. **Cross-Agent File Mutation Queue Lock (`FILE_MUTATION_LOCKS`)**:
   - Process-wide path-based queue mutex in `thunder-agent-loop`. Serializes concurrent write operations to the same physical file across parallel agents or simultaneous tool calls, preventing race conditions and silent overwrites.
5. **Deterministic Zero-Latency Plugin Activation**:
   - `thunder-agent-root` uses a deterministic baseline (session conversation and skills permanently active) paired with keyword intent triggers, eliminating secondary LLM classification latency and token costs.
6. **Production-Grade STDIO Sidecar Daemon (`thunder-agent-daemon`)**:
   - Zero port conflicts and lifecycle naturally bound to the parent process (Electron/desktop UI). Features concurrency semaphore scheduling, backpressured ordered STDOUT streaming, cooperative pausing, and human-in-the-loop question bubbles (`ask_user_question`).

---

## 📦 Workspace Package Matrix (11 Workspace Crates)

| Crate | Path | Description |
| :--- | :--- | :--- |
| **`thunder-agent-loop`** | [`thunder-agent-loop`](thunder-agent-loop) | **Agent A**: Single-agent atomic loop engine with decoupled tool output eviction and cross-agent file write mutex |
| **`thunder-pi-bridge`** | [`thunder-pi-bridge`](thunder-pi-bridge) | **LLM Transport**: Pre-bundled Node.js sidecar running `@earendil-works/pi-ai`, normalizing model dialects and streaming |
| **`thunder-agent-providers`** | [`thunder-agent-providers`](thunder-agent-providers) | **Model Catalog**: Loads `models.json` / `auth.json`, manages model specifications and thinking level mappings |
| **`thunder-agent-skills`** | [`thunder-agent-skills`](thunder-agent-skills) | **Skills Engine**: Discovers and parses Playbooks / SKILL.md with intent trigger matching and 120s TTL global cache |
| **`thunder-agent-plugin`** | [`thunder-agent-plugin`](thunder-agent-plugin) | **TS Plugin Host**: Single-file TypeScript plugin runner with native execution, blue-green reload, and error immunity |
| **`thunder-agent-mcp`** | [`thunder-agent-mcp`](thunder-agent-mcp) | **MCP Client**: Standard JSON-RPC 2.0 client for discovering and bridging remote Model Context Protocol tools |
| **`thunder-agent-root`** | [`thunder-agent-root`](thunder-agent-root) | **Microkernel Host**: Dynamic plugin assembler with trigger routing, executing prompts into structured run results |
| **`thunder-agent-daemon`** | [`thunder-agent-daemon`](thunder-agent-daemon) | **STDIO Sidecar**: Daemon for Electron and desktop UIs with role permissions, concurrency semaphores, pause, and ask-user |
| **`thunder-conversation`** | [`thunder-agent-core/conversation`](thunder-agent-core/conversation) | **Conversation Store**: Atomic filesystem and in-memory session persistence, `index.json` fast index, and multi-topology tracking |
| **`thunder-orchestra`** | [`thunder-agent-core/thunder-orchestra`](thunder-agent-core/thunder-orchestra) | **Agent B**: Multi-agent scheduler with Sequential, Parallel, Fan-Out, role prompt injection, and result synthesis |
| **`thunder-tui`** | [`thunder-agent-core/tui`](thunder-agent-core/tui) | **Interactive TUI**: Claude Code style full-width terminal interface with live streaming, reasoning fold, and orchestra monitor |

---

## 🚀 Quickstart

### Prerequisites
- **Rust**: 1.80+
- **Node.js**: 20+ (Required for `thunder-pi-bridge` sidecar; runtime bundle is pre-packaged, no `npm install` needed)

### Common Commands

```bash
# 1. Clone workspace
git clone https://github.com/lucacicii/thunder.git
cd thunder

# 2. Launch interactive TUI
./run.sh

# 3. Launch STDIO Sidecar daemon (for desktop / Electron integration)
./daemon.sh

# 4. Run workspace test suite across all 11 packages
./test.sh
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
