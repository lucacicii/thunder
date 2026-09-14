# ⚡ Thunder TUI

> **Claude Code 风格交互式终端界面 — 基于 Ratatui & Crossterm**

[English](README_en.md) | [简体中文](README.md)

`thunder-tui` 是 Thunder Agent 体系的交互式终端客户端，提供极简全宽终端流、交互式斜杠命令系统、实时流式 Token 渲染、思考过程展开与会话恢复管理等全套功能。

---

## 🌟 核心特性

- **Claude Code 风格无框全宽布局**：
  - **顶部状态栏 (Header)**：实时显示连接模型、运行状态（Idle / Thinking / Streaming / Tool）与会话标识。
  - **全宽对话流 (Chat Stream)**：
    - Markdown 排版渲染与打字机效果（`TokenDelta`）。
    - 深度思考链折叠展示（`ReasoningDelta`，支持 DeepSeek-R1 / o1 类模型）。
    - 工具调用与执行结果卡片（`BashTool`、`ReadFileTool`、`WriteFileTool`、`McpToolBridge` 等）。
  - **交互式 `❯` 提示行与斜杠命令自动补全**：输入 `/` 即时呼出命令菜单，支持 `Tab` 补全与 `↑/↓` 切换。
  - **会话恢复与持久化 (`/resume`)**：随时列出历史会话，按序号或 ID 一键恢复继续对话。
- **全键盘极速交互**：
  - `Enter`：发送指令 / 启动 Agent 循环。
  - `Tab`：自动补全斜杠命令 / 切换焦点。
  - `Ctrl + N`：新建会话。
  - `Ctrl + P`：快速轮转编排模式。
  - `Ctrl + M`：切换编排监控视图。
  - `Ctrl + H`：弹出快捷键帮助浮层。
  - `Ctrl + C` / `Esc`：终止当前运行中的 Agent 或退出 TUI。
- **崩溃安全恢复**：内置终端 Panic Hook 保证终端始终能平稳恢复 Normal 模式，杜绝终端乱码或光标丢失。

---

## ⚡ 交互式斜杠命令 (详见 [KEYBOARD.md](KEYBOARD.md))

| 命令 | 说明 |
| :--- | :--- |
| **`/resume [# \| id]`** | 列出全部历史会话，或按编号/ID恢复会话 |
| **`/help`** | 呼出所有命令与快捷键参考 |
| **`/model [name]`** | 查看当前模型或热切换模型（如 `gpt-4o`, `claude-3-7-sonnet` 等） |
| **`/mode [auto\|pipe\|par\|single]`** | 切换执行拓扑模式 |
| **`/skills [list\|load\|scan]`** | 浏览技能库目录、读取 Playbook 提示词或扫描目录 |
| **`/mcp [list\|servers\|reload]`** | 列出已连接的 MCP 服务及动态工具，或重载配置 |
| **`/compact`** | 自动压缩历史上下文，提取会话摘要并裁剪老旧轮次 |
| **`/stats`** | 查看当前会话统计（消息数、对话轮次、Token 消耗） |
| **`/config [key] [val]`** | 查看或修改运行时配置（`temperature`, `max_turns` 等） |
| **`/workspace [path]`** | 查看当前工作区或切换至新的工作目录 |
| **`/export [path]`** | 将当前对话与工具调用链完整导出为 Markdown 报告 |
| **`/clear`** | 清空当前对话流并创建全新空白会话 |
| **`/health`** | 运行多智能体与环境健康检查探针 |
| **`/pipeline <task>`** | 直接以串行流水线模式运行任务（Planner ➔ Coder） |
| **`/parallel <task>`** | 直接以并行评审模式运行任务（Planner + Reviewer） |
| **`/quit`** | 优雅退出 Thunder TUI |

---

## 🚀 快速启动

```bash
# 1. 使用一键脚本启动 (推荐)
./run.sh

# 2. 离线 Mock 演示模式
cargo run -p thunder-tui --bin thunder-tui -- --mock
```

---

## 🧪 自动化测试

```bash
# 运行全生态测试套件
./test.sh
```
