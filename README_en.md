# Thunder

> **Ultra-lightweight, High-Performance, Modular Rust AI Agent Workspace & Runtime**

[English](README_en.md) | [简体中文](README.md)

Thunder is a modern AI Agent ecosystem built in Rust. Managed under a single Git monorepo and a unified Cargo workspace, it provides an end-to-end architecture spanning ultra-fast single-agent execution units, unified LLM transport, and dynamic microkernel plugin composition (conversation, skills, MCP).

Hosted at [lucacicii/thunder](https://github.com/lucacicii/thunder). For monorepo layout, architecture dependencies, and development conventions, see **[WORKSPACE.md](WORKSPACE.md)** (or **[WORKSPACE_en.md](WORKSPACE_en.md)**).

---

## ⚡ Core Architectural Pillars

1. **Single-Agent Closed-Loop Kernel (`thunder-agent-loop`)**:
   - Atomic closed execution unit for a single task. Responsible for driving multi-turn reasoning, streaming token parsing, concurrent tool execution, and context pruning. Never couples multi-agent graph or shared blackboard logic.
   - Tool calls are dispatched concurrently (`ToolExecutor::execute_all`), so a single agent already gains parallel tool-execution benefits while keeping a stable conversation prefix to maximize provider-side prompt cache hits.
2. **In-Process Streaming Transport (`rpi-ai`)**:
   - Model calls are HTTP streams issued from inside `thunder-agent-providers` — **no child process, no bundled `bridge.mjs`, no Node required on the host**.
   - Covers `openai-responses`, `openai-completions` and `anthropic-messages`, including token accounting, thinking-level and prompt-cache retention mapping. Dialects outside those three (Gemini, Bedrock, Vertex, Mistral) are rejected with a clear error rather than failing silently.
3. **Checkpoint Context Compaction**:
   - Between two compactions the request prefix stays **byte-identical**, maximizing provider-side prompt cache hits.
   - Only when approaching the model's real window limit (`max_context_tokens - reserve_tokens`) does it swap older history for a single LLM-generated structured checkpoint (Goal / Progress / Decisions / Next Steps + read/modified file lists), keeping the last `keep_recent_tokens` verbatim.
   - Falls back to an emergency trim (atomically dropping the oldest complete turns) when summarization is unavailable; the raw pre-compaction transcript is preserved via `AgentRunResult.raw_messages`.
4. **Cross-Agent File Mutation Queue Lock (`FILE_MUTATION_LOCKS`)**:
   - Process-wide path-based queue mutex in `thunder-agent-loop`. Serializes concurrent write operations to the same physical file across parallel agents or simultaneous tool calls, preventing race conditions and silent overwrites.
5. **Deterministic Zero-Latency Plugin Activation**:
   - `thunder-agent-root` uses a deterministic baseline (session conversation and skills permanently active) paired with keyword intent triggers, eliminating secondary LLM classification latency and token costs.
6. **Project Config & Long-Term Memory (`.thunder/`)**:
   - Thunder reads its own `.thunder/` project config, fully isolated from `.arp` (owned by the panel): `config.json` is merged by layer (user / project / local) and can append a system-prompt file and cap turns; `THUNDER.md` and `memory/*.md` are injected as long-term memory; `mcp.json` auto-registers MCP servers.
   - The memory block sits in the request prefix and is read once per host init, keeping the provider-side prompt cache prefix stable.
   - **The user data root is host-relocatable**: `THUNDER_CONFIG_DIR` (or the `--config-dir <path>` binary flag) owns all of thunder's user-level state — `models.json`, `auth.json`, `models_metadata.json`, `scratchpad/`, `logs/`, `conversations/`, `config.json`, `mcp.json`, `THUNDER.md`, `skills/`, `permissions.json` (the "always allow" rules the host agreed to) — defaulting to `~/.thunder`. Point it at a private directory and an embedding host (Electron, a scheduler) gets a fully isolated instance. The variable's meaning was promoted from "config file directory" to "full data root": **anyone already setting it will see conversations, scratchpad and the rest move there too**. Cross-tool paths (`~/.agents`, `~/.pi`, the shell rc that holds API keys) are unaffected and stay on the real home.
7. **One Tool-Execution Funnel, a Trace of Every Call, and an Unloadable Approval-Mode Plugin**:
   - The kernel has exactly **one** way to run a tool, `ToolExecutor::run_tool`: cancellation → guard → scope → admission → prompt → execute → trace. Built-in tools, Rust plugin tools, MCP tools and runtime remote tools all pass through it, so **no tool can run untraced**.
   - A trace is produced at **every exit of every step**, and records the *decision* itself — who refused, which scope the call landed in, what the user answered — redacted before it reaches disk (secret key names, secret shapes, document bodies reduced to a digest and a preview). It cannot be turned off.
   - The approval policy (`plan` / `ask` / `acceptEdits` / `yolo`) lives in an **unloadable host plugin**; the kernel keeps only a `ToolGate` hook, and **no plugin installed means everything but the guard is allowed** — the headless-host red line holds structurally.
8. **Production-Grade STDIO Sidecar Daemon (`thunder-agent-daemon`)**:
   - Zero port conflicts and lifecycle naturally bound to the parent process (Electron/desktop UI). Features concurrency semaphore scheduling, backpressured ordered STDOUT streaming, cooperative pausing, and human-in-the-loop question bubbles (`ask_user_question`).

---

## 📦 Workspace Package Matrix (10 Workspace Crates)

| Crate | Path | Description |
| :--- | :--- | :--- |
| **`thunder-agent-loop`** | [`thunder-agent-loop`](thunder-agent-loop) | **Agent A**: Single-agent atomic loop engine with decoupled tool output eviction and cross-agent file write mutex |
| **`thunder-agent-pack-code`** | [`thunder-agent-pack-code`](thunder-agent-pack-code) | **Code Pack**: Six built-in tools (`bash`/`read_file`/`write_file`/`grep`/`find`/`ls`), atomic writes, and path scope classification (`CodeScope`) |
| **`thunder-agent-runtime`** | [`thunder-agent-runtime`](thunder-agent-runtime) | **Headless Runtime**: Drives `AgentLoop` over a JSONL protocol for embedding hosts with no UI |
| **`thunder-agent-providers`** | [`thunder-agent-providers`](thunder-agent-providers) | **Model Catalog**: Loads `models.json` / `auth.json`, manages model specifications and thinking level mappings |
| **`thunder-agent-skills`** | [`thunder-agent-skills`](thunder-agent-skills) | **Skills Engine**: Discovers and parses Playbooks / SKILL.md with intent trigger matching and 120s TTL global cache |
| **`thunder-agent-mcp`** | [`thunder-agent-mcp`](thunder-agent-mcp) | **MCP Client**: Standard JSON-RPC 2.0 client for discovering and bridging remote Model Context Protocol tools |
| **`thunder-agent-root`** | [`thunder-agent-root`](thunder-agent-root) | **Microkernel Host**: Dynamic plugin assembler with trigger routing, executing prompts into structured run results |
| **`thunder-agent-daemon`** | [`thunder-agent-daemon`](thunder-agent-daemon) | **STDIO Sidecar**: Daemon for Electron and desktop UIs with approval modes (plan / ask / acceptEdits / yolo) and **mode-change events** (a pushed `mode_changed` frame), concurrency semaphores, pause, and ask-user |
| **`thunder-conversation`** | [`thunder-agent-core/conversation`](thunder-agent-core/conversation) | **Conversation Store**: Atomic filesystem and in-memory session persistence, `index.json` fast index, and multi-topology tracking |
| **`thunder-tui`** | [`thunder-agent-core/tui`](thunder-agent-core/tui) | **Interactive TUI**: Claude Code style full-width terminal interface with live streaming and reasoning fold |

---

## 🚀 Quickstart

### Prerequisites
- **Rust**: 1.80+
- **No Node.js needed**: model calls happen in-process (the `bridge.mjs` sidecar that used to require it is gone)

### Common Commands

```bash
# 1. Clone workspace
git clone https://github.com/lucacicii/thunder.git
cd thunder

# 2. Launch interactive TUI
./run.sh
#    Release build by default (the render path is an order of magnitude faster);
#    for a fast edit-build loop use THUNDER_TUI_PROFILE=tui-dev ./run.sh

# 3. Launch STDIO Sidecar daemon (for desktop / Electron integration)
./daemon.sh

# 4. Run workspace test suite across all 10 packages
./test.sh
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
