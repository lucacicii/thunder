# `.arp/`

Thunder 工作区的项目级 Agent Resume 配置与共享产物目录。参考
[`agent-resume-panel`](https://github.com/lucacicii/agent-resume-panel) 的 `.arp/` 约定建立。

## 目录

| 路径 | 内容 |
| --- | --- |
| `config.json` | 项目级配置（已提交，供 Workbench / 外部 Agent 读取） |
| `docs/` | 跨会话共享的产物与决策记录（需要时再建） |
| `roles/` | 项目级角色定义 `*.md`（需要时再建，供 Desktop 扫描） |

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
