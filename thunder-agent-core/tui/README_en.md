# ⚡ Thunder TUI

> **Interactive Terminal User Interface — Powered by Ratatui & Crossterm**

[English](README_en.md) | [简体中文](README.md)

`thunder-tui` is the official terminal user interface for the Thunder Agent ecosystem, providing interactive chatting, multi-agent orchestra monitoring, real-time token streaming, reasoning chain expansion, and session switching.

---

## 🌟 Key Features

- **Modern Responsive Layout (Ratatui 0.29 + Crossterm)**:
  - **Header Bar**: Live model name, agent execution status (Idle / Thinking / Streaming / Tool), token counter.
  - **Chat Stream**:
    - Markdown-styled dialogue flow.
    - Real-time token streaming typing effect (`TokenDelta`).
    - Chain-of-thought reasoning block (`ReasoningDelta` for DeepSeek-R1 / o1).
    - Tool execution cards (`BashTool`, `ReadFileTool`, `WriteFileTool`).
  - **Sidebar**: Saved session list with instant loading.
  - **Orchestra Monitor**: Topology pipeline diagram and stage drill-downs.
- **Keyboard-Driven Workflow**:
  - `Enter`: Send message / trigger agent loop.
  - `Tab`: Cycle focus between Input / Chat / Sidebar.
  - `Ctrl + N`: New conversation session.
  - `Ctrl + B`: Toggle sidebar.
  - `Ctrl + M`: Toggle Orchestra Monitor.
  - `Ctrl + H`: Help overlay modal.
  - `Ctrl + C` / `Esc`: Cancel running agent or quit.
- **Crash-Safe Terminal Handling**: Custom panic hook ensures terminal always restores cleanly.

---

## ⌨️ Keyboard Shortcuts Reference (See [KEYBOARD.md](KEYBOARD.md))

| Shortcut | Action | Description |
| :--- | :--- | :--- |
| **`Enter`** | **Send / Execute** | Submit prompt and trigger agent loop |
| **`Tab`** | **Cycle Focus** | Cycle focus: `Input` ➔ `Chat` ➔ `Sidebar` ➔ `Input` |
| **`Ctrl + N`** | **New Session** | Create and open a clean conversation session |
| **`Ctrl + P`** | **Cycle Mode** | Cycle: `Single` ➔ `🚀 Pipeline` ➔ `⚖️ Parallel` |
| **`Ctrl + B`** | **Toggle Sidebar** | Expand / collapse saved sessions sidebar |
| **`Ctrl + M`** | **Orchestra Monitor** | Toggle multi-agent orchestra topology monitor |
| **`Ctrl + H`** | **Help Modal** | Toggle keyboard shortcuts overlay |
| **`Up / Down`** | **History / Scroll** | History in Input, scroll in Chat, select in Sidebar |
| **`Home / End`** | **Top / Bottom** | Scroll to top / snap to bottom of chat |
| **`d / Delete`** | **Delete Session** | Delete selected session in Sidebar |
| **`Ctrl + C / Esc`** | **Cancel / Quit** | Interrupt running agent or exit TUI |

---

## 🚀 Quickstart

```bash
# 1. One-click bash script (Recommended)
cd thunder-agent/tui
./run.sh

# 2. Via test.sh runner
./test.sh run
./test.sh mock    # Offline mock mode

# 3. Via direct Cargo command
cargo run --bin thunder-tui -- --mock
cargo run --bin thunder-tui -- --session sess_1787560000000
```

---

## 🧪 Testing

```bash
# Run TUI tests
./test.sh
```
