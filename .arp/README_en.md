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
- Scopes use short crate / module names (`loop`, `core`, `daemon`, `providers`, `skills`, `mcp`, `root`, `tui`, `conversation`), not full `thunder-agent-*` names and not file paths.
- All commits and pushes happen at the workspace root. Never run `git init` inside sub-crates (see [`../WORKSPACE.md`](../WORKSPACE.md)).
