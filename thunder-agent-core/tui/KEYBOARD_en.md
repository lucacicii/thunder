# ⚡ Thunder TUI Keyboard & Slash Commands Guide

[English](KEYBOARD_en.md) | [简体中文](KEYBOARD.md)

Thunder TUI adopts a minimalist, modern full-screen terminal interface inspired by **Claude Code**, combining a full-width chat stream with an interactive `❯` prompt line and real-time slash command auto-completion popup.

---

## ⚡ 1. Interactive Slash Commands

Type `/` in the bottom `❯ ` input line to open the command palette:

| Command | Aliases | Parameters | Description |
| :--- | :--- | :--- | :--- |
| **`/resume`** | `/sessions`, `/load_session` | `[# \| id]` | **List all saved sessions or resume by index/ID** |
| **`/help`** | `/?` | | Show command reference and keyboard shortcut overlay |
| **`/model`** | `/m` | `[name]` | View active model or hot-switch models (e.g. `deepseek-v4.1-flash`, `gpt-4o`, `claude-3-7-sonnet`) |
| **`/mode`** | `/topology` | `[auto \| single]` | Switch execution mode (plugin host / direct single agent) |
| **`/skills`** | `/skill`, `/sk` | `[list \| load <name> \| scan]` | Browse skill directory, load playbook instructions, or trigger rescan |
| **`/mcp`** | `/server`, `/tools` | `[list \| servers \| reload]` | List connected MCP servers & tools, or reload configuration |
| **`/compact`** | `/prune`, `/compress` | | Manually compress context, extract summary, and prune older turns |
| **`/stats`** | `/cost`, `/tokens`, `/usage` | | Show session metrics (messages, turns, tool calls, estimated tokens) |
| **`/config`** | `/settings`, `/cfg` | `[key] [val]` | View or update runtime settings (`temperature`, `max_turns`, `timeout_ms`, etc.) |
| **`/workspace`** | `/cwd`, `/dir` | `[path]` | View current workspace directory or switch to a new path |
| **`/export`** | `/save`, `/dump` | `[path]` | Export entire conversation and tool execution chain to Markdown |
| **`/clear`** | `/new`, `/reset` | | Clear current chat stream and initialize a blank session |
| **`/health`** | `/doctor`, `/status` | | Execute multi-agent and environment health check probes |
| **`/pipeline`** | `/seq` | `<task>` | Run task directly in sequential pipeline mode (Planner ➔ Coder) |
| **`/parallel`** | `/par`, `/council` | `<task>` | Run task directly in multi-role parallel review mode (Planner + Reviewer) |
| **`/fanout`** | `/decompose` | `<task>` | Run task directly in decomposed subtask fan-out mode |
| **`/quit`** | `/exit`, `/q` | | Cleanly exit Thunder TUI |

---

## ⌨️ 2. Global Keyboard Shortcuts

| Shortcut | Action | Description |
| :--- | :--- | :--- |
| **`Enter`** | **Send / Submit** | Submit input prompt or execute slash command |
| **`Tab`** | **Auto-complete / Focus** | Auto-complete slash command; otherwise toggle focus between Input and Chat |
| **`↑ / ↓`** | **Select / History** | Navigate candidate list in slash popup; browse prompt history in normal state |
| **`Ctrl + N`** | **New Session** | Clear chat stream and create a fresh blank session |
| **`Ctrl + P`** | **Cycle Mode** | Fast-cycle execution mode: `Auto ➔ Single` |
| **`Ctrl + H`** | **Help Modal** | Toggle keyboard shortcut help card overlay |
| **`Ctrl + C`** | **Cancel / Exit** | Cancel active agent task if running; cleanly exit if idle |
| **`Esc`** | **Close / Dismiss** | Dismiss command palette, close help modal, or cancel running task |
| **`PageUp / PageDown`** | **Scroll Page** | Scroll chat stream up or down by 10 lines |
| **`Home / End`** | **Top / Bottom** | `Home` scrolls to top of chat; `End` jumps to bottom and resumes **auto-follow** |
