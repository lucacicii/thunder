# ⚡ Thunder Agent Pack Code

> **Thunder Agent's code capability pack: shell/file tools, workspace path scope, atomic writes, change reporting, and the engineering default prompt**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-pack-code` is a **capability pack** for `thunder-agent-loop`. The kernel is generic: it streams, runs tools, prunes context and stops — and it deliberately does **not** know what a shell, a workspace or a file is. Those belong here, in the pack a *coding* host installs. A business host (a prototype editor, a form bot) installs a different pack — or none at all — and none of this comes along for the ride.

---

## 🚀 Highlights

1. **Six built-in tools**: `bash`, `read_file`, `write_file`, `grep`, `find`, `ls`. Every one is `with_default_cwd(workspace)`, so a relative path in a call means "relative to the workspace" rather than "relative to the process's current directory".
2. **`CodeScope`: location, not judgement**:
   - `classify(target, effect)` only answers *where* a call lands — `InScope` / `OutOfScope` / `Unknown` / `Critical`.
   - It may only **relax, never tighten**: `Unknown` earns no automatic approval, so it becomes a question rather than a silent refusal.
   - `Exec` and `Other` always come back `Unknown`: the honest classification of `rm -rf $BUILD_DIR` is "this file cannot tell".
   - Symbolic links are resolved by canonicalizing each existing segment — the repo's only escape check.
3. **Atomic writes with process-wide per-path locking**:
   - A staging file in the target's own directory → `write_all` → `sync_all` (flush before the rename, so a crash never leaves a correctly-named empty file) → restore permissions → `rename`.
   - `FILE_MUTATION_LOCKS` queues on the **physical** path (the canonicalized path, or "canonicalized parent + file name"), so relative, absolute and symlinked-parent spellings of the same file share one lock.
   - The cancellation token races the write: a cancelled run leaves the target either untouched or completely written.
   - The lock is **in-process**: two daemons, or a daemon and the TUI, do not coordinate through it.
4. **Change reporting**:
   - `write_file` reports after every write; `bash` takes a `git status --porcelain` snapshot before and after and diffs them — a shell command's file side effects cannot be read off its arguments, which is why this is the one place a snapshot is still needed.
   - The result is a custom event the kernel does not interpret: `file_change` (the `FILE_CHANGE` constant — **do not rename it**, it is a wire contract).
5. **The engineering default prompt**: `DEFAULT_AUTONOMOUS_SYSTEM_PROMPT` — "understand before acting". Planning is not a phase the prompt asks for; it is a **mechanism the user controls**.

---

## 🛠️ Quick start

```rust
use thunder_agent_loop::prelude::*;
use thunder_agent_pack_code::{install_code_pack, CodePackConfig};

let config = CodePackConfig {
    register_builtins: true,
    apply_default_prompt: false,
};

// Installs the scope plus the six built-in tools; each tool gets the
// workspace as its default cwd.
let agent = install_code_pack(agent, &config);
```

Every piece is public, so you can install just one part:

```rust
use thunder_agent_pack_code::{builtin::WriteFileTool, CodeScope};

let agent = agent.with_tool_scope(std::sync::Arc::new(
    CodeScope::from_agent_config(&config, workspace.as_deref()),
));
agent.register_tool(std::sync::Arc::new(
    WriteFileTool::default().with_default_cwd(ws.clone()),
));
```

---

## 🧰 Tool reference

| Tool | Arguments | Notes |
|---|---|---|
| `bash` | `command`, `cwd?`, `timeout_ms?` | **120s** default timeout; `bash -c`; stdin nulled so an interactive prompt cannot hang the run; environment scrubbing (names containing `KEY`/`TOKEN`/`SECRET`/`PASSWORD`/`AUTH`/`CREDENTIAL`, or prefixed `AWS_`/`GITHUB_`/`OPENAI_`/`ANTHROPIC_`/`DEEPSEEK_`, are removed unless listed in `THUNDER_ENV_PASSTHROUGH`); on Unix `process_group(0)` plus a whole-group kill on timeout |
| `read_file` | `path`, `offset`, `limit` | **10 MB** single-read ceiling |
| `write_file` | `path`, `content` | Atomic write + per-path lock (above) |
| `grep` | `pattern`, `path`, … | ripgrep's engine embedded **in-process** (`grep-searcher` + `grep-regex` + `ignore`) — no child process; caps at 100 matching lines / 500 chars per line / 50 KB of output |
| `find` | `pattern`, `path` | `ignore`-crate walk (respects `.gitignore`); caps at 1000 entries / 50 KB |
| `ls` | `path` | 1000 entries per directory |

When output exceeds the threshold the kernel's registry spills it to the scratchpad — that applies to **every** tool, pack or no pack, which is why the pack does not wrap the registry.

---

## 📄 Known details

- `install_code_pack` has **no production caller**: both `thunder-agent-root` and the TUI register each tool themselves and install `CodeScope` separately. The convenience function is currently exercised only by tests.
- `CodePackConfig::apply_default_prompt` is declared but **never read**; the default prompt is in practice taken explicitly by the host (`DEFAULT_AUTONOMOUS_SYSTEM_PROMPT`).
- `CodeScope::new`'s doc comment says "with the pack's default protections in place", but the constructor leaves `protections` / `write_blocked` empty; the protections are actually applied by `from_agent_config` → `with_thunder_data_root`. Doc and implementation disagree.
- `read_file`'s 10 MB ceiling only applies when `limit` is absent, and the read is a full-file read followed by a line slice — "chunking" bounds the output, not the memory.

---

## 📄 License

Licensed under the [Apache License 2.0](../LICENSE).
