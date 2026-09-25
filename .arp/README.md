# `.arp/`

[English](README_en.md) | [简体中文](README.md)

Thunder 工作区的项目级 Agent Resume 配置与共享产物目录。参考
[`agent-resume-panel`](https://github.com/lucacicii/agent-resume-panel) 的 `.arp/` 约定建立。

## 目录

| 路径 | 内容 |
| --- | --- |
| `config.json` | 项目级配置（已提交，供 Workbench / 外部 Agent 读取） |
| `docs/` | 跨会话共享的产物与决策记录（需要时再建） |
| `roles.jsonl` | 项目级角色定义，**一行一个 role**（需要时再建，供 Desktop 与 Rust host 读取） |

## `config.json`

Schema 版本 `1`。字段语义见 agent-resume-panel 的
`docs/desktop/settings-and-data.md` § 项目级 `.arp/config.json`。

- `shared`：跨模块项目事实（语言、显示名等）。目前为预留槽位。
- `workbench.git.commitMessage`：覆盖「设置 → Workbench」的提交信息默认值。
  - `style`：`conventional` | `gitmoji` | `custom`
  - `language`：提交信息输出语言
  - `customInstructions`：`style: custom` 时的格式规则
  - `extraInstructions`：追加到所选风格之后的项目特有规则

缺省分组表示**未配置**，不是空覆盖。**不要**把 API Key、主题或 panel home 写进本文件。

## 本仓库约定

- 提交信息：Conventional Commits，**英文**描述（`language: en`）。
- scope 用 crate / 模块短名（`loop`、`core`、`daemon`、`providers`、`skills`、`mcp`、
  `root`、`tui`、`orchestra`、`conversation`），不用 `thunder-agent-*` 全名，也不用文件路径。
- 提交、推送都在工作区根目录完成，不要在子 crate 里再 `git init`（见 [`../WORKSPACE.md`](../WORKSPACE.md)）。

## `roles.jsonl`（项目级角色）

角色是**配置**——一个 persona 加一个能力档位——不是代码。Rust host 是权限的
**唯一权威**；Desktop 只是读同一批文件做展示。

作用域（后者覆盖前者，按 `id` 去重）：

| 作用域 | 路径 |
| --- | --- |
| 全局 | `~/.thunder/roles.jsonl`（受 `THUNDER_CONFIG_DIR` 覆盖） |
| 项目 | `<project>/.arp/roles.jsonl` |

**一行一个 role。** 坏的/半截的行会被跳过，不会破坏整份列表。

```json
{"id":"plan","name":"Plan","aliases":["p"],"persona":["你是一个只做规划的工程助手。","禁止修改文件、禁止执行 shell。"],"permission":"read","askUser":true,"exitGate":true,"enabled":true}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | string | 斜杠命令名（`/plan`）；唯一键，项目级覆盖全局 |
| `name` | string? | 显示名，缺省用 `id` |
| `aliases` | string[]? | 额外斜杠别名 |
| `description` | string? | 斜杠面板里的一句话说明 |
| `persona` | string \| string[]? | 提示词正文；数组按行拼接（更易读） |
| `permission` | `"read" \| "write" \| "bash"`? | 能力档，默认 `read` |
| `model` / `thinkingLevel` | string? | 覆盖本次运行的模型 / 思考档 |
| `askUser` | bool? | 是否挂载 `ask_user_question`（决定气泡可用） |
| `exitGate` | bool? | 退出该模式是否需要用户确认 |
| `enabled` | bool? | 默认 `true`；关掉即不进斜杠列表 |
| `triggers` | string[]? | 可选：无斜杠时的关键词匹配 |

权限档是递进的：`read ⊂ write ⊂ bash`。

| 档位 | `read_file` | `write_file` | `bash` |
| --- | :---: | :---: | :---: |
| `read` | ✅ | ❌ | ❌ |
| `write` | ✅ | ✅ | ❌ |
| `bash` | ✅ | ✅ | ✅ |

被禁的工具**根本不会注册**，所以模型连工具名都看不到。
