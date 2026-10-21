# Thunder Workspace

[English](WORKSPACE_en.md) | [简体中文](WORKSPACE.md)

How the code is organised, which way dependencies point, and how to verify a change. Architecture details live in [`docs/architecture.md`](docs/architecture.md).

---

## 1. One repository

- GitHub: <https://github.com/lucacicii/thunder>
- A single Git monorepo and a single Cargo workspace: the root `Cargo.toml` governs 12 members with one shared `Cargo.lock`.
- The git root is the workspace root; do not `git init` inside a crate.
- Crates no longer carry their own `LICENSE` / `.gitignore`: those are root-level assets, inherited through `[workspace.package]`.

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
```

---

## 2. Layout

```text
thunder/
├── README.md / README_en.md            # front page
├── WORKSPACE.md / WORKSPACE_en.md      # this file
├── Cargo.toml                          # workspace (12 members) + shared deps
├── rust-toolchain.toml                 # pinned 1.98.0 (same as CI)
├── run.sh / daemon.sh / test.sh        # TUI / sidecar / tests
├── scripts/check-version.py            # version consistency gate
├── docs/
│   ├── architecture.md                 # architecture and module map
│   ├── permission-structure.md         # permissions, approvals, the four modes
│   └── migration-0.6.md                # migrating 0.5 → 0.6
├── .github/workflows/                  # CI (fmt / clippy / version / test) and release
├── .thunder/                           # project config (mirrors the user-level ~/.thunder)
├── .arp/                               # owned by agent-resume-panel; thunder never reads it
├── examples/electron/                  # Electron sidecar example
├── thunder-agent-types/                # pure wire types
├── thunder-agent-loop/                 # kernel
├── thunder-agent-policy/               # policy layer (tool funnel / gate / UI / read-only)
├── thunder-agent-pack-code/            # reference capability pack
├── thunder-agent-providers/            # model catalog + in-process streaming
├── thunder-agent-skills/               # skill parsing and injection
├── thunder-agent-mcp/                  # MCP client / transports / bridge
├── thunder-agent-root/                 # host assembly
├── thunder-agent-daemon/               # NDJSON sidecar
├── thunder-agent-runtime/              # JSONL headless runtime
└── thunder-agent-core/
    ├── conversation/                   # conversation stores (thunder-conversation)
    └── tui/                            # terminal UI (thunder-tui)
```

---

## 3. Dependency direction

Internal dependencies are path dependencies; git URLs across crates are not allowed.

| Crate | Internal crates |
|---|---|
| `thunder-agent-types` | — |
| `thunder-agent-loop` | `types` (`policy` is `[dev-dependencies]` only) |
| `thunder-agent-policy` | `types`, `loop` |
| `thunder-agent-pack-code` | `types`, `policy`, `loop` |
| `thunder-agent-providers` | `types`, `policy`, `loop` |
| `thunder-agent-skills` | `types`, `policy`, `loop` |
| `thunder-agent-mcp` | `types`, `policy`, `loop` |
| `thunder-conversation` | `loop` |
| `thunder-agent-runtime` | `types`, `policy`, `loop`, `providers` |
| `thunder-agent-root` | `types`, `policy`, `loop`, `pack-code`, `providers` |
| `thunder-agent-daemon` | `types`, `policy`, `loop`, `providers`, `skills`, `mcp`, `conversation`, `root` |
| `thunder-tui` | `types`, `policy`, `loop`, `pack-code`, `providers`, `skills`, `mcp`, `conversation`, `root` |

> In `root`, `conversation` / `skills` / `mcp` are **optional features** (all on by default), so the table lists only the mandatory edges; turning a feature off removes that component from `StandardHostBuilder`.

**Layering**: `types` is the leaf; `loop` knows only `types` and its own seams; `policy` implements the kernel's `Tools` seam (the edge points policy → loop, never the reverse); `root` is assembly; `tui` / `daemon` / `runtime` are the top-level binaries.

### 3.1 Five design rules

1. **The kernel holds no policy**: no gate, no UI, no approval, no MCP inside `thunder-agent-loop`. It calls `Tools` and `LLMClientTrait`, and guarantees cancellation, timeouts and the pairing invariant.
2. **Hosts assemble explicitly**: one run's contribution is four lists — `PromptSection` / `ToolPack` / `Observer` / `Prepared`. Registered is active; `plugins.disabled` is the only subtraction.
3. **One tool funnel**: `ToolExecutor::run_tool` is the only entry (guard → mode → dialog → interceptors → registry → trace). Model calls and plugin reverse-RPC both go through it.
4. **Concurrent writes are safe**: file writes take a per-physical-path lock (`FILE_MUTATION_LOCKS` in `pack-code/src/builtin/fs.rs`); the kernel holds no lock.
5. **Config lives in `.thunder/`, never `.arp/`**: `.arp/` belongs to agent-resume-panel. The user-level data root can be relocated wholesale with `THUNDER_CONFIG_DIR` (or `--config-dir`); resolution lives in `thunder-agent-loop/src/core/paths.rs`.

---

## 4. Running and testing

Rust 1.80+ (`rust-version`); the repository pins 1.98.0. No Node.js required.

```bash
./run.sh          # thunder-tui
./daemon.sh       # thunder-agent-daemon
./test.sh         # workspace tests

cargo test -p thunder-agent-loop
cargo test -p thunder-conversation
cargo check --workspace
cargo build --workspace --release
```

The three gates (same as CI):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features thunder-agent-daemon/testing-mock -- -D warnings
python3 scripts/check-version.py
```

---

## 5. Commit conventions

- Commit at the workspace root; no nested repositories.
- Conventional Commits with English verbs: `feat(root): …`, `fix(policy): …`, `refactor(loop): …`, `docs: …`, `test(loop): …`.
- Before committing, run the gates in §4 and `cargo test --workspace`.
