# Thunder Workspace Architecture Guide

[English](WORKSPACE_en.md) | [简体中文](WORKSPACE.md)

This document describes the codebase topology, crate dependency relationships, and development and testing standards for the Thunder workspace.

---

## 1. Unified Repository

GitHub Repository:

https://github.com/lucacicii/thunder

The entire ecosystem is governed as a **single Git monorepo** with a **unified Cargo workspace** (a single root `Cargo.toml` managing 11 crates and sharing a single `Cargo.lock`), eliminating nested repositories or git submodules.

- Git root is this workspace root.
- Remote points to `origin → lucacicii/thunder`.
- All commits, branches, tags, and pushes happen in the monorepo root. Never run `git init` in subdirectories.

Clone:

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
```

---

## 2. Directory Layout

```text
thunder/                          # Git monorepo root & Cargo workspace root
├── README.md                     # Chinese entry document
├── README_en.md                  # English entry document
├── WORKSPACE.md                  # Chinese workspace guide
├── WORKSPACE_en.md               # English workspace guide
├── Cargo.toml                    # Unified workspace config (11 crates)
├── Cargo.lock                    # Unified lockfile
├── run.sh                        # Launch TUI terminal
├── daemon.sh                     # Launch STDIO Sidecar daemon
├── test.sh                       # Workspace-wide test suite script
├── thunder-agent-loop/           # Agent A: Single-agent atomic loop engine
├── thunder-pi-bridge/            # pi-ai Node Sidecar transport (@earendil-works/pi-ai)
├── thunder-agent-providers/      # LLM catalog & config parsing
├── thunder-agent-skills/         # Skill parsing, discovery & global cache
├── thunder-agent-plugin/         # TypeScript single-file plugin engine
├── thunder-agent-mcp/            # MCP client & dynamic tool bridge
├── thunder-agent-root/           # Microkernel host for dynamic plugin composition
├── thunder-agent-daemon/         # STDIO Sidecar daemon (Electron / desktop integration)
└── thunder-agent-core/           # Core modules collection
    ├── conversation/             # Session & Turn storage (thunder-conversation)
    ├── thunder-orchestra/        # Agent B: Multi-agent scheduler
    └── tui/                      # Interactive terminal UI (thunder-tui)
```

---

## 3. Dependencies & Architectural Layering

All workspace crates utilize **Path dependencies**. Git URLs or external repository links between internal crates are strictly prohibited.

```text
                       ┌─────────────────────────┐
                       │    thunder-pi-bridge    │ (Node Sidecar: pi-ai)
                       └────────────┬────────────┘
                                    │ implements LLMClientTrait
                                    ▼
┌────────────────────┐   ┌───────────────────────────┐
│ thunder-agent-loop │ ◄─┤  thunder-agent-providers  │
└─────────┬──────────┘   └─────────────┬─────────────┘
          │ (A: Unit)                  │
          ▼                            │
┌──────────────────────────────────────┴─────────────────────────────────┐
│ Plugin Ecosystem: skills / plugin / mcp / conversation / orchestra    │
└──────────────────────────────────────┬─────────────────────────────────┘
                                       │
                                       ▼
                             ┌────────────────────┐
                             │ thunder-agent-root │ (Host: Microkernel)
                             └─────────┬──────────┘
                                       │
                      ┌────────────────┴────────────────┐
                      ▼                                 ▼
              ┌───────────────┐               ┌──────────────────────┐
              │  thunder-tui  │               │ thunder-agent-daemon │
              │ (Terminal UI) │               │   (STDIO Sidecar)    │
              └───────────────┘               └──────────────────────┘
```

### Core Design Principles

1. **A/B Contract Boundary**:
   - **A = `thunder-agent-loop`**: Owns atomic execution for a single AgentLoop (multi-turn reasoning, streaming events, tool execution, context pruning). Has no awareness of multi-agent topologies, outer schedulers, or session stores.
   - **B = `thunder-orchestra`**: Coordinates multiple A units (start, await, cancel, pipeline handoffs, task decomposition, and result synthesis). B **never drives A's internal turns**.
2. **Pure Transport Abstraction & Pi-Bridge**:
   - `thunder-agent-loop` defines `LLMClientTrait` as a pure transport contract without internal HTTP or dialect dependencies.
   - In production, calls are delegated through `thunder-pi-bridge` to `@earendil-works/pi-ai`, achieving zero-maintenance support for multi-provider dialects and reasoning effort levels.
3. **Concurrent Mutation Safety**:
   - Whether under multi-agent parallelism (`Parallel` / `FanOut`) or concurrent tool calls within a single unit, file write mutations are serialized via `FILE_MUTATION_LOCKS` using process-wide physical path mutexes, preventing silent overwrites.
4. **Deterministic Lightweight Host**:
   - `thunder-agent-root` maintains standard baseline plugins (conversation + skills) and dynamically activates higher-order plugins via keyword intent triggers, avoiding expensive secondary LLM classification latency.

---

## 4. Running & Testing

### Prerequisites
- **Rust**: 1.80+
- **Node.js**: 20+

### Scripts

```bash
./run.sh          # Launch thunder-tui interactive terminal
./daemon.sh       # Launch thunder-agent-daemon sidecar
./test.sh         # Execute all tests across all 11 workspace crates
```

### Individual Package Commands

```bash
# Test specific crates
cargo test -p thunder-agent-loop
cargo test -p thunder-orchestra
cargo test -p thunder-conversation
cargo test -p thunder-agent-daemon

# Workspace-wide checks and builds
cargo check --workspace
cargo test --workspace
cargo build --workspace --release
```

---

## 5. Development Conventions

- **Single Repository**: All development is committed to the workspace root. Never create nested `.git` repositories in subpackages.
- **Commit Format**: Conventional Commits in English (e.g. `feat(orchestra): ...`, `refactor(loop): ...`).
- **Continuous Integration**: Ensure `./test.sh` passes 100% locally before pushing.
