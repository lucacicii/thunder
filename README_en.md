# Thunder

> **Ultra-lightweight, High-Performance, Modular Rust AI Agent Workspace & Runtime**

[English](README_en.md) | [简体中文](README.md)

Thunder is a modern AI Agent ecosystem built in Rust. Managed under a single Git monorepo and a unified Cargo workspace, it provides an end-to-end architecture spanning ultra-fast single-agent execution units, unified LLM transport, and dynamic microkernel plugin composition (conversation, skills, MCP, TS script plugins).

Hosted at [lucacicii/thunder](https://github.com/lucacicii/thunder). For monorepo layout, architecture dependencies, and development conventions, see **[WORKSPACE.md](WORKSPACE.md)** (or **[WORKSPACE_en.md](WORKSPACE_en.md)**).

---

## ⚡ Core Architectural Pillars

1. **Single-Agent Closed-Loop Kernel (`thunder-agent-loop`)**:
   - Atomic closed execution unit for a single task. Responsible for driving multi-turn reasoning, streaming token parsing, concurrent tool execution, and context pruning. Never couples multi-agent graph or shared blackboard logic.
   - Tool calls are dispatched concurrently (`ToolExecutor::execute_all`), so a single agent already gains parallel tool-execution benefits while keeping a stable conversation prefix to maximize provider-side prompt cache hits.
2. **`pi-bridge` Single Transport Convergence**:
   - Replaced high-maintenance custom Rust model dialect adapters with a single Node.js sidecar running `@earendil-works/pi-ai`, pre-bundled via esbuild.
   - Zero `npm install` required on fresh checkouts. Seamlessly unifies dialect handling, token accounting, and thinking level mappings across OpenAI, Anthropic Claude, DeepSeek, Google Gemini, Ollama, and more.
3. **Checkpoint Context Compaction**:
   - Between two compactions the request prefix stays **byte-identical**, maximizing provider-side prompt cache hits.
   - Only when approaching the model's real window limit (`max_context_tokens - reserve_tokens`) does it swap older history for a single LLM-generated structured checkpoint (Goal / Progress / Decisions / Next Steps + read/modified file lists), keeping the last `keep_recent_tokens` verbatim.
   - Degrades to mechanical compaction when summarization fails; the raw pre-compaction transcript is preserved via `AgentRunResult.raw_messages`.
4. **Cross-Agent File Mutation Queue Lock (`FILE_MUTATION_LOCKS`)**:
   - Process-wide path-based queue mutex in `thunder-agent-loop`. Serializes concurrent write operations to the same physical file across parallel agents or simultaneous tool calls, preventing race conditions and silent overwrites.
5. **Deterministic Zero-Latency Plugin Activation**:
   - `thunder-agent-root` uses a deterministic baseline (session conversation and skills permanently active) paired with keyword intent triggers, eliminating secondary LLM classification latency and token costs.
6. **Production-Grade STDIO Sidecar Daemon (`thunder-agent-daemon`)**:
   - Zero port conflicts and lifecycle naturally bound to the parent process (Electron/desktop UI). Features concurrency semaphore scheduling, backpressured ordered STDOUT streaming, cooperative pausing, and human-in-the-loop question bubbles (`ask_user_question`).

---

## 📦 Workspace Package Matrix (10 Workspace Crates)

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
| **`thunder-conversation`** | [`thunder-agent-core/conversation`](thunder-agent-core/conversation) | **Session Store**: Atomic Fs storage with in-memory dual mode, `index.json` fast indexing, and topology stage tracking |
| **`thunder-tui`** | [`thunder-agent-core/tui`](thunder-agent-core/tui) | **Interactive TUI**: Claude Code style full-width terminal interface with live streaming and reasoning fold |

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

# 4. Run workspace test suite across all 10 packages
./test.sh
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
