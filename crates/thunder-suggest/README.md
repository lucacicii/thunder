# 💡 thunder-suggest

`thunder-suggest` 是 Thunder 的启发式思考与后续追问建议扩展包（Heuristic Follow-up Suggestions & Ghost Text）。

它负责在 Agent 每一轮任务结束或会话空闲时，根据当前上下文动态生成 1~3 条最有价值的后续追问建议，并在 TUI 和 Daemon 端提供无缝的输入补全体验。

---

## 核心职责

1. **启发式规则引擎 (`heuristic.rs`)**：
   * **零延迟、零 Token 开销**：深度分析上一轮所调用的工具与修改的文件：
     * **修改了代码**（`write_file`, `edit_block`）$\to$ 建议：*“为刚才修改的代码编写配套单元测试”*、*“查看刚才修改引起的 git diff 并检查语法”*；
     * **起草了计划**（`write_plan`）$\to$ 建议：*“退出 Plan 模式并开始执行实施方案”*；
     * **退出了计划**（`exit_plan_mode`）$\to$ 建议：*“开始落地执行方案中的 Step 1”*；
     * **运行了诊断**（`doctor`）$\to$ 建议：*“根据诊断报告修复缺失的依赖与配置”*；
     * **工具报错** $\to$ 建议：*“分析刚才的错误堆栈并尝试自动修复”*；
     * **只读检索**（`grep`, `find`, `read_file`）$\to$ 建议：*“基于检索到的代码逻辑编写落地代码”*。

2. **轻量 LLM 个性化建议 (`llm_suggest.rs`)**：
   * 可选接入极速的小模型（4s 超时，max_tokens = 80），根据最后一次用户问题和助手回答提炼出个性化的后续问题。

3. **双端交互呈现 (`engine.rs`)**：
   * **桌面端（`thunder-daemon`）**：在 `DaemonResponse::TaskCompleted` 响应中携带 `suggested_questions: Vec<String>`，前端渲染为快捷胶囊按钮（Chip），点击即上屏；
   * **终端（`thunder-tui`）**：
     * 输入框为空时渲染淡灰色 **幽灵占位符（Ghost Text）**：`💡 <建议文本> (Tab: Accept · ↑/↓: Switch)`；
     * **按 `Tab` 键**：秒级将当前建议填入输入框，光标自动跳转到末尾；
     * **按 `↑` / `↓` 键**：在候选建议之间循环切换。
