# ⚡ Thunder TUI 全键盘与斜杠命令指南 (Keyboard & Slash Commands Guide)

[English](KEYBOARD_en.md) | [简体中文](KEYBOARD.md)

Thunder TUI 采用类似 **Claude Code** 的极简现代全屏终端界面，全宽对话流结合交互式 `❯` 提示行与实时斜杠命令自动补全浮窗。

---

## ⚡ 1. 交互式斜杠命令 (Slash Commands)

在底部 `❯ ` 提示行输入 `/` 即可触发实时命令补全浮窗：

| 命令 | 别名 | 参数 | 功能说明 |
| :--- | :--- | :--- | :--- |
| **`/resume`** | `/sessions`, `/load_session` | `[# \| id]` | **列出所有历史会话，或按序号/ID一键恢复继续对话** |
| **`/help`** | `/?` | | 呼出命令参考与键盘快捷键说明 |
| **`/model`** | `/m` | `[name]` | 查看当前模型或热切换模型（如 `deepseek-v4.1-flash`, `gpt-4o`, `claude-3-7-sonnet`） |
| **`/mode`** | `/topology` | `[auto \| pipe \| par \| fanout \| single]` | 切换执行拓扑模式（自动路由 / 串行流水线 / 并行评审 / 任务拆解分片 / 单 Agent） |
| **`/skills`** | `/skill`, `/sk` | `[list \| load <name> \| scan]` | 浏览技能库目录、读取 Playbook 提示词或重新扫描外部目录 |
| **`/mcp`** | `/server`, `/tools` | `[list \| servers \| reload]` | 列出已连接的 MCP 服务及动态工具，或重载 MCP 配置文件 |
| **`/compact`**| `/prune`, `/compress` | | 自动压缩历史上下文，提取会话摘要并裁剪老旧轮次以节约 Token |
| **`/stats`** | `/cost`, `/tokens`, `/usage` | | 查看当前会话统计（消息数、对话轮次、工具调用数、估计 Token 消耗） |
| **`/config`** | `/settings`, `/cfg` | `[key] [val]` | 查看或修改运行时配置（`temperature`, `max_turns`, `timeout_ms` 等） |
| **`/workspace`**| `/cwd`, `/dir` | `[path]` | 查看当前工作区或切换至新的工作目录 |
| **`/export`** | `/save`, `/dump` | `[path]` | 将当前对话与工具调用链完整导出为 Markdown 报告 |
| **`/clear`** | `/new`, `/reset` | | 清空当前对话流并创建全新空白会话 |
| **`/health`** | `/doctor`, `/status` | | 运行多智能体与环境健康检查探针 |
| **`/pipeline`**| `/seq` | `<task>` | 直接以串行流水线模式运行任务（Planner ➔ Coder） |
| **`/parallel`**| `/par`, `/council` | `<task>` | 直接以多角色并行评审模式运行任务（Planner + Reviewer） |
| **`/fanout`** | `/decompose` | `<task>` | 直接以子任务拆解分片模式并发运行任务 |
| **`/quit`** | `/exit`, `/q` | | 优雅退出 Thunder TUI |

---

## ⌨️ 2. 全局键盘快捷键 (Global Shortcuts)

| 快捷键 | 功能 | 说明 |
| :--- | :--- | :--- |
| **`Enter`** | **发送 / 提交** | 提交输入框中的 Prompt，或执行斜杠命令 |
| **`Tab`** | **自动补全 / 切换焦点** | 在输入 `/` 时自动补全命令；其他时候在 Input 与 Chat 之间切换焦点 |
| **`↑ / ↓`** | **选择候选 / 历史命令** | 在命令补全弹窗中上下选择；在普通状态下浏览输入历史记录 |
| **`Ctrl + N`** | **新建会话** | 清空当前对话流，创建并初始化一个新的空白 Session |
| **`Ctrl + P`** | **轮转编排模式** | 在 `Auto ➔ Pipeline ➔ Parallel ➔ FanOut ➔ Single` 之间快速轮转 |
| **`Ctrl + M`** | **切换编排监视器** | 在标准聊天界面与 Multi-Agent Orchestra 拓扑监视器之间切换 |
| **`Ctrl + H`** | **帮助弹窗** | 弹出 / 关闭键盘快捷键帮助速查卡片 |
| **`Ctrl + C`** | **取消执行 / 退出** | 若 Agent 正在运行则**取消当前任务**；若处于空闲状态则退出 |
| **`Esc`** | **关闭弹窗 / 清空** | 关闭补全浮窗、帮助窗口或取消运行中的任务 |
| **`PageUp / PageDown`** | **快速翻页** | 快速上下滚动对话流 10 行 |
| **`Home / End`** | **顶底跳转** | `Home` 滚到对话顶端，`End` 瞬间滚回消息最底部并恢复**自动流式跟随** |
