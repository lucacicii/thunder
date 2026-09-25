# ⚡ Thunder TUI

> **Claude Code Style Interactive Terminal Interface — Powered by Ratatui & Crossterm**

[English](README_en.md) | [简体中文](README.md)

`thunder-tui` is the official interactive terminal client for the Thunder Agent ecosystem. Deeply integrated with `thunder-agent-root` and `thunder-orchestra`, it provides a full-width chat stream, interactive slash command system, real-time token streaming typewriter effect, collapsible reasoning chains, multi-agent orchestra monitor, and synthesis resolution reports.

---

## 🚀 Key Features

- **Claude Code Frameless Full-Width Layout**:
  - **Header Bar**: Displays active model name, execution status (Idle / Thinking / Streaming / Tool), estimated token counter, and current session ID.
  - **Chat Stream**:
    - Markdown formatted rendering with smooth typewriter token streaming (`TokenDelta`).
    - Collapsible deep reasoning chains (`ReasoningDelta`, supporting DeepSeek-R1, o1, etc.).
    - Tool invocation and execution result cards (`BashTool`, `ReadFileTool`, `WriteFileTool`, `McpToolBridge`).
    - **Multi-Agent Synthesis Report Rendering**: In `Parallel` or `FanOut` topologies, prominently renders `### 📝 Synthesized Report` at the top with expandable per-role execution details.
  - **Interactive `❯` Input Line & Slash Auto-completion**: Type `/` to open the command palette popup with `Tab` completion and `↑/↓` selection.
  - **Session Resume & Persistence (`/resume`)**: List all saved sessions and restore any session by index or ID instantly.
- **Keyboard-Driven Workflow**:
  - `Enter`: Submit prompt / run agent loop.
  - `Tab`: Auto-complete slash command / switch focus.
  - `Ctrl + N`: Create clean new session.
  - `Ctrl + P`: Fast cycle topology modes (`Auto` ➔ `Pipeline` ➔ `Parallel` ➔ `FanOut` ➔ `Single`).
  - `Ctrl + M`: Toggle orchestra topology monitor.
  - `Ctrl + H`: Toggle keyboard shortcut help popup.
  - `Ctrl + C` / `Esc`: Cancel running task or exit TUI cleanly.
- **Crash-Safe Terminal Handling**: Panic hooks guarantee clean restoration to Normal terminal mode on unexpected errors.

---

## ⚡ Interactive Slash Commands (See [KEYBOARD.md](KEYBOARD.md) / [KEYBOARD_en.md](KEYBOARD_en.md))

| Command | Arguments | Description |
| :--- | :--- | :--- |
| **`/resume`** | `[# \| id]` | List saved sessions or restore by index/ID |
| **`/help`** | | Display command reference and keyboard shortcuts |
| **`/model`** | `[name]` | View active model or hot-switch models (e.g. `deepseek-v4.1-flash`, `gpt-4o`, `claude-3-7-sonnet`) |
| **`/mode`** | `[auto \| pipe \| par \| fanout \| single]` | Switch orchestration topology |
| **`/skills`** | `[list \| load \| scan]` | Browse skill directory, load playbook instructions, or rescan |
| **`/mcp`** | `[list \| servers \| reload]` | List connected MCP servers and tools, or reload configs |
| **`/compact`** | | Compress context, extract conversation summary, and trim older turns |
| **`/stats`** | | Display session metrics (message count, turns, token consumption) |
| **`/config`** | `[key] [val]` | View or modify runtime options (`temperature`, `max_turns`, etc.) |
| **`/workspace`** | `[path]` | View active directory or switch workspace |
| **`/export`** | `[path]` | Export chat and tool execution log to Markdown |
| **`/clear`** | | Clear current chat stream and create a fresh session |
| **`/health`** | | Run orchestrator and environment health checks |
| **`/pipeline`** | `<task>` | Run task directly in sequential pipeline mode (Planner ➔ Coder) |
| **`/parallel`** | `<task>` | Run task directly in multi-role parallel review mode (Planner + Reviewer) |
| **`/fanout`** | `<task>` | Run task directly in decomposed subtask fan-out mode |
| **`/quit`** | | Cleanly exit Thunder TUI |

---

## 🚀 Quickstart

```bash
# 1. Launch via root script (Recommended)
./run.sh

# 2. Offline mock demo mode
cargo run -p thunder-tui --bin thunder-tui -- --mock

# 3. Resume specific session
cargo run -p thunder-tui --bin thunder-tui -- --session sess_1790253029573
```

---

## 🛠️ Testing

```bash
# Run TUI tests
cargo test -p thunder-tui
```
