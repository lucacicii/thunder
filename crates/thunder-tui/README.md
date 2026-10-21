# 🖥 thunder-tui

`thunder-tui` 是 Thunder 的交互式终端用户界面（bin: `thunder-tui`）。

基于 `ratatui` 与 `crossterm` 构建，直接驱动 `thunder-core` 微内核，提供高对比度、无跳动、响应灵敏的开发者交互体验。

---

## 核心交互功能

1. **三栏终端布局 (`ui.rs`)**：
   * **顶部状态栏**：当前工作目录、选中的模型标识、模式状态芯片（绿色 `[YOLO]` / 黄色 `[PLAN]`）、当前快照版本 `vN` 及总工具数；
   * **中部时间线**：流式对话文本（Markdown 语法高亮）、Reasoning 思考折叠流、工具调用卡片（带耗时与执行状态：`[⏳ RUN]`、`[✓ OK]`、`[✖ ERR]`）、系统通知；
   * **底部输入与状态栏**：支持多行编辑，运行中实时显示 `Steer Active Run (Enter to inject)` 与 Token 估算。

2. **快捷操作与按键映射**：
   * `Enter`：发送提示词 / 运行中实时插话（通过 `SteerQueues` 注入下一轮）；
   * `Shift + Tab`（`BackTab`）：快速在 `yolo` 与 `plan` 模式之间切换；
   * `Ctrl + P`：暂停 / 恢复任务（在工具边界协作式暂停）；
   * `Ctrl + C` / `Esc`：取消当前正在运行的任务 / 清空输入；
   * `Up` / `Down`：滚动浏览时间线。

3. **内置斜杠命令 (Slash Commands)**：
   * `/model [id]`：查看可用模型或快速切换模型；
   * `/tools`：查看当前工具快照中的所有核心、MCP 与脚本工具及版本；
   * `/mode [yolo|plan]`：设置会话权限模式；
   * `/new`：开启新会话；
   * `/sessions`：查看最近保存的会话列表；
   * `/load <id>`：恢复加载指定的历史会话；
   * `/clear`：清空时间线展示；
   * `/help`：查看命令帮助；
   * `/quit` / `/exit`：退出 TUI。

---

## 启动方式

```bash
# 通过配套脚本启动
./run.sh

# 或通过环境变量/参数指定模型与工作区
MODEL="lucastech/zai/glm-5.3" cargo run -p thunder-tui --bin thunder-tui -- --workspace /path/to/project
```
