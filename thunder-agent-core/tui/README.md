# ⚡ Thunder TUI

> **Claude Code 风格交互式终端界面 — 基于 Ratatui & Crossterm**

[English](README_en.md) | [简体中文](README.md)

`thunder-tui` 是 Thunder Agent 体系的官方交互式终端客户端。它深度集成 `thunder-agent-root`，提供全宽对话流、交互式斜杠命令系统、实时流式打字机效果以及深度思考链折叠展示。

---

## 🚀 核心特性

- **Claude Code 风格无框全宽布局**：
  - **顶部状态栏 (Header)**：实时显示连接模型、执行模式、思考档位与运行状态（Idle / Thinking / Streaming / Tool）。
  - **底部会话栏 (Footer)**：输入框正下方常驻一行显示当前会话标题（标题还是占位符时显示 session id，自动生成标题后立即更新）；临时状态消息（`✔`、队列、暂停）临时叠在它上方一行。
  - **全宽对话流 (Chat Stream)**：
    - Markdown 格式化排版渲染与打字机效果（`TokenDelta`）。
    - 深度思考链折叠展示（`ReasoningDelta`，支持 DeepSeek-R1、o1 等模型）。
    - 工具调用与执行结果卡片（`BashTool`、`ReadFileTool`、`WriteFileTool`、`McpToolBridge` 等）。
    - **多 Agent 聚合决议报告渲染**：当运行 `Parallel` 或 `FanOut` 拓扑时，优先突出渲染 `### 📝 Synthesized Report` 综合报告，并支持展开各角色的分工输出卡片。
  - **交互式 `❯` 提示行与斜杠命令自动补全**：输入 `/` 即时呼出浮窗命令菜单，支持 `Tab` 补全与 `↑/↓` 切换。
  - **会话恢复与持久化 (`/resume`)**：随时列出历史会话，按序号或 ID 一键恢复继续对话。
  - **Turn 时间轴轨道**：对话区右缘悬浮一条极简轨道，一次用户提问一个刻度；鼠标悬停看详情（状态 / 工具数 / 耗时 / 提问摘要），单击或 `Enter` 把该轮滚到顶部；`Ctrl + T` 开关，窄终端自动隐藏。
- **全键盘极速交互**：
  - `Enter`：发送指令 / 启动 Agent 循环。
  - `Tab`：自动补全斜杠命令 / 切换输入框焦点。
  - `Ctrl + N`：新建会话。
  - `Ctrl + P`：快速轮转执行模式（`Auto` ➔ `Single`）。
  - `Ctrl + H`：弹出快捷键帮助浮层（支持 `↑ / ↓`、`PageUp / PageDown` 滚动）。
  - `Ctrl + T`：显示 / 隐藏对话区右缘的 turn 时间轴轨道（`Tab` 聚焦，`j / k` + `Enter` 跳转）。
  - `Ctrl + A`：全选输入框内容（配合 `Ctrl + C` 复制到系统剪贴板）。
  - `Ctrl + C`：有选区时复制；否则清空输入；否则终止运行中的任务；空闲时退出。
  - `Esc`：先取消选区，否则关闭浮层或终止运行中的任务。
- **崩溃安全恢复**：内置终端 Panic Hook 保证终端在发生异常时平稳恢复 Normal 模式，杜绝终端乱码或光标丢失。

---

## ⚡ 交互式斜杠命令 (详见 [KEYBOARD.md](KEYBOARD.md))

| 命令 | 参数 | 说明 |
| :--- | :--- | :--- |
| **`/resume`** | `[# \| id]` | 列出全部历史会话，或按编号/ID恢复会话 |
| **`/help`** | | 呼出所有命令与快捷键参考 |
| **`/model`** | `[name]` | 查看当前模型或热切换模型（如 `deepseek-v4.1-flash`, `gpt-4o`, `claude-3-7-sonnet`） |
| **`/mode`** | `[auto \| single]` | 切换执行模式（插件宿主 / 单 Agent 直连） |
| **`/skills`** | `[list \| load \| scan]` | 浏览技能库目录、读取 Playbook 提示词或重新扫描目录 |
| **`/mcp`** | `[list \| servers \| reload]` | 列出已连接的 MCP 服务及动态工具，或重载配置 |
| **`/compact`** | | 自动压缩历史上下文，提取会话摘要并裁剪老旧轮次 |
| **`/stats`** | | 查看当前会话统计（消息数、对话轮次、Token 消耗） |
| **`/config`** | `[key] [val]` | 查看或修改运行时配置（`temperature`, `max_turns` 等） |
| **`/workspace`** | `[path]` | 查看当前工作区或切换至新的工作目录 |
| **`/export`** | `[path]` | 将当前对话与工具调用链完整导出为 Markdown 报告 |
| **`/clear`** | | 清空当前对话流并创建全新空白会话 |
| **`/health`** | | 运行多智能体与环境健康检查探针 |
| **`/timeline`** | `[on \| off]` | 显示 / 隐藏对话区右缘的 turn 时间轴轨道 |
| **`/pipeline`** | `<task>` | 直接以串行流水线模式运行任务（Planner ➔ Coder） |
| **`/parallel`** | `<task>` | 直接以多角色并行评审模式运行任务（Planner + Reviewer） |
| **`/fanout`** | `<task>` | 直接以子任务拆解分片模式并发运行任务 |
| **`/quit`** | | 优雅退出 Thunder TUI |

---

## 🚀 快速启动

```bash
# 1. 使用一键脚本启动 (推荐)
./run.sh

# 2. 恢复指定历史会话
cargo run -p thunder-tui --bin thunder-tui -- --session sess_1790253029573
```

---

## 🛠️ 自动化测试

```bash
# 运行 TUI 单元与集成测试
cargo test -p thunder-tui
```

---

## ⚡ 性能与渲染架构

长会话（数百分支、数百 KB 的 `conversation.json`）中滚动或输入不应引起 CPU 飙高。
`thunder-tui` 通过三层机制保证这一点：

- **按需重绘**：主循环不再每 50ms 无条件 `draw`。仅当有事件、缓存失效或处于动画状态
  （运行中 / 暂停 / 有排队 / 有瞬时状态）时才请求新帧，空闲时 CPU 占用接近 0。
- **转录缓存 (Transcript Cache)**：渲染前先命中 `TranscriptCache`（键覆盖终端宽度、
  raw/markdown 开关、消息与流式片段数量、spinner 帧等）。命中时复用已排版的行，
  仅重新绘制**可见视口**内的行；未命中时才整篇重建。
- **事件去抖**：鼠标 `Moved` 事件在队列中合并，仅当时间轴悬停目标真正变化时才触发重绘；
  终端事件在专用线程阻塞读取，不再轮询。

### 构建档位

`./run.sh` 默认使用 **release** 构建（debug 版渲染路径慢约一个数量级，长会话下会明显发烫）。
需要调试或快速迭代时可用：

```bash
THUNDER_TUI_PROFILE=tui-dev ./run.sh   # 已优化，但链接只需数秒
THUNDER_TUI_PROFILE=dev     ./run.sh   # 未优化，便于调试
```
