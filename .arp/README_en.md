# `.arp/`

> **Project-level Agent Resume configuration & shared artifact directory for the Thunder workspace**

[English](README_en.md) | [简体中文](README.md)

This directory follows the `.arp/` convention established by
[`agent-resume-panel`](https://github.com/lucacicii/agent-resume-panel).

## Directory Contents

| Path | Content |
| --- | --- |
| `config.json` | Project-level configuration (committed; read by the Workbench / external agents) |
| `docs/` | Cross-session shared artifacts and decision records (created as needed) |
| `roles.jsonl` | Project-level role definitions, **one role per line** (created as needed; read by Desktop & the Rust host) |

## `config.json`

Schema version `1`. Field semantics mirror the agent-resume-panel
`docs/desktop/settings-and-data.md` § project-level `.arp/config.json`.

- `shared`: Cross-module project facts (language, display name, etc.). Currently a reserved slot.
- `workbench.git.commitMessage`: Overrides the default commit message behavior in “Settings → Workbench”.
  - `style`: `conventional` | `gitmoji` | `custom`
  - `language`: Commit message output language
  - `customInstructions`: Formatting rules applied when `style: custom`
  - `extraInstructions`: Additional project-specific rules appended after the selected style

Missing groups mean **unconfigured**, not an empty override. **Never** put API keys, themes, or panel home paths in this file.

## Repository Conventions

- Commit messages: Conventional Commits with **English** descriptions (`language: en`).
- Scopes use short crate / module names (`loop`, `core`, `daemon`, `providers`, `skills`, `mcp`, `root`, `tui`, `orchestra`, `conversation`), not full `thunder-agent-*` names and not file paths.
- All commits and pushes happen at the workspace root. Never run `git init` inside sub-crates (see [`../WORKSPACE.md`](../WORKSPACE.md)).

## `roles.jsonl` (Project-Level Roles)

Roles are **configuration** — a persona plus a capability tier — not code. The Rust host is the
**single source of truth** for permissions; Desktop only reads the same files for display.

Scopes (later overrides earlier, deduplicated by `id`):

| Scope | Path |
| --- | --- |
| Global | `~/.thunder/roles.jsonl` (overridable via `THUNDER_CONFIG_DIR`) |
| Project | `<project>/.arp/roles.jsonl` |

**One role per line.** Malformed or partial lines are skipped without breaking the rest of the list.

```json
{"id":"plan","name":"Plan","aliases":["p"],"persona":["You are a planning-only engineering assistant.","Never modify files, never run shell commands."],"permission":"read","askUser":true,"exitGate":true,"enabled":true}
```

| Field | Type | Description |
| --- | --- | --- |
| `id` | string | Slash command name (`/plan`); unique key; project scope overrides global |
| `name` | string? | Display name; defaults to `id` |
| `aliases` | string[]? | Additional slash aliases |
| `description` | string? | One-line description shown in the slash panel |
| `persona` | string \| string[]? | Prompt body; arrays are joined per line (more readable) |
| `permission` | `"read" \| "write" \| "bash"`? | Capability tier; defaults to `read` |
| `model` / `thinkingLevel` | string? | Overrides the model / thinking level for runs using this role |
| `askUser` | bool? | Whether `ask_user_question` is mounted (controls bubble availability) |
| `exitGate` | bool? | Whether exiting this mode requires user confirmation |
| `enabled` | bool? | Defaults to `true`; disabled roles are excluded from the slash list |
| `triggers` | string[]? | Optional keyword matching when no slash command is used |

Permission tiers are progressive: `read ⊂ write ⊂ bash`.

| Tier | `read_file` | `write_file` | `bash` |
| --- | :---: | :---: | :---: |
| `read` | ✅ | ❌ | ❌ |
| `write` | ✅ | ✅ | ❌ |
| `bash` | ✅ | ✅ | ✅ |

Blocked tools are **never registered**, so the model cannot even see their names.
