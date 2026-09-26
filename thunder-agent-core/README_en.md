# ⚡ Thunder Agent Core

> **Conversation Persistence & Terminal Application Packages**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-core` groups together the session persistence managers and terminal user interface packages in the Thunder ecosystem. All subpackages are managed directly under the unified root Cargo Workspace.

```text
thunder-agent-core/
├── conversation/               # [Sub-package] Session & Conversation Storage (thunder-conversation)
└── tui/                        # [Sub-package] Claude Code Style Terminal UI (thunder-tui)
```

---

## 📦 Subpackage Index

- **[`conversation`](./conversation)**: Session management subpackage (`thunder-conversation`). Manages session lifecycles, Turn groupings, and Memory/Fs dual persistence with `index.json` caching and atomic shadow writes, plus topology stage tracking metadata (Sequential stages, Parallel branches, Delegate subtasks).
- **[`tui`](./tui)**: Interactive terminal user interface (`thunder-tui`). Built with Ratatui 0.29 & Crossterm, integrated with `thunder-agent-root`, featuring live token streaming (`TokenDelta`), collapsible reasoning chains (`ReasoningDelta`), and a complete keyboard-driven workflow.

---

## 🛠️ Testing

```bash
# Test thunder-conversation
cargo test -p thunder-conversation

# Test thunder-tui
cargo test -p thunder-tui
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
