# ⚡ Thunder Agent Core

> **Multi-Agent Orchestration, Conversation Persistence & Terminal Application Packages**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-core` groups together the core orchestration schedulers, session persistence managers, and terminal user interface packages in the Thunder ecosystem. All subpackages are managed directly under the unified root Cargo Workspace.

```text
thunder-agent-core/
├── conversation/               # [Sub-package] Session & Conversation Storage (thunder-conversation)
├── thunder-orchestra/          # [Sub-package] Multi-Agent Scheduler (thunder-orchestra)
└── tui/                        # [Sub-package] Claude Code Style Terminal UI (thunder-tui)
```

---

## 📦 Subpackage Index

- **[`thunder-orchestra`](./thunder-orchestra)**: Multi-Agent Scheduler (Scheduler B). Composes [`thunder-agent-loop`](../../thunder-agent-loop) units, supporting `Single` (direct query), `Sequential` (pipeline handoffs), `Parallel` (multi-perspective council review), and `FanOut` (structured subtask decomposition), with an integrated `Synthesizer` node for unified consensus summaries and trace persistence.
- **[`conversation`](./conversation)**: Session management subpackage (`thunder-conversation`). Manages single-agent and multi-agent session lifecycles (Sequential stage tracking, Parallel branch merges, Delegate subtask drill-downs), Turn groupings, and Memory/Fs dual persistence with `index.json` caching and atomic shadow writes.
- **[`tui`](./tui)**: Interactive terminal user interface (`thunder-tui`). Built with Ratatui 0.29 & Crossterm, integrated with `thunder-agent-root`, featuring live token streaming (`TokenDelta`), collapsible reasoning chains (`ReasoningDelta`), multi-agent orchestra monitor views, synthesized consensus rendering, and a complete keyboard-driven workflow.

---

## 🛠️ Testing

```bash
# Test thunder-orchestra
cargo test -p thunder-orchestra

# Test thunder-conversation
cargo test -p thunder-conversation

# Test thunder-tui
cargo test -p thunder-tui
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
