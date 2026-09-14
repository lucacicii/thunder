# ⚡ Thunder Agent Core Monorepo

> **Multi-Agent Orchestration, Conversation & Applications**

[English](README_en.md) | [简体中文](README.md)

---

## 📦 Monorepo Architecture & Sub-packages

`thunder-agent-core` houses multi-agent orchestration schedulers, session stores, and user terminal interfaces.

```
thunder-agent-core/
├── Cargo.toml                  # Workspace root configuration
├── conversation/               # [Sub-package] Session & Conversation Management (thunder-conversation)
├── thunder-orchestra/          # [Sub-package] Multi-Agent Scheduler (thunder-orchestra)
├── tui/                        # [Sub-package] Terminal User Interface (thunder-tui)
└── ...                         # Extensible for future agent sub-packages
```

### Sub-package Index

- **[`tui`](./tui)**: Interactive terminal user interface (`thunder-tui`). Built with Ratatui 0.29 & Crossterm, integrated with `thunder-agent-root`, supporting interactive single-agent chatting, multi-agent orchestra monitoring, real-time token streaming (`TokenDelta`), reasoning chain rendering (`ReasoningDelta`), and full keyboard navigation.
- **[`conversation`](./conversation)**: Conversation management sub-package (`thunder-conversation`). Manages single & multi-agent conversation lifecycles, Turn groupings, Memory/Fs dual-engine atomic persistence (with `index.json` caching), and orchestration topologies (Sequential stages, Parallel branches, Delegate drill-downs).
- **[`thunder-orchestra`](./thunder-orchestra)**: Multi-Agent Scheduler (Scheduler B). Built on top of [`thunder-agent-loop`](../thunder-agent-loop) units, supporting sequential pipeline handoffs (Pipeline Sequential), parallel execution (Parallel), agent delegation (`DelegateTool`), and run history persistence (`RunStore`).

---

## 🛠️ Quickstart

### Run Scheduler Tests

```bash
# Run workspace tests from monorepo root
cargo test --workspace

# Run thunder-orchestra test suite
cd thunder-orchestra
./test.sh
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
