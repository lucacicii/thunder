# 📋 thunder-plan

`thunder-plan` 是 Thunder 的 Plan Mode（规划模式）治理扩展包。

作为一个独立的 **In-Process Crate**，它将“静态只读 Shell 证明器”、“专用计划工具”以及“只读安全中间件”封装在一起，为宿主（`thunder-daemon`、`thunder-tui`）提供开箱即用的只读治理能力，同时确保 `thunder-core` 保持微内核纯粹。

---

## 核心职责

1. **静态只读 Shell 命令分析器 (`read_only.rs`)**：
   * 基于词法分析，悲观证明一条 Shell 命令行是否 100% 只读。
   * **绝对拦截**：只要出现未加引号的分隔符（`;`、`&`、`|`、`>`、`<`、`$()`、反引号、括号子 shell 等），一律判定为非只读并拒绝。
   * **只读动词白名单**：放行 `git status/diff/log`、`grep`、`rg`（排除 `--pre`）、`find`（排除 `-delete` 等）、`cat`、`ls`、`head`、`tail`、`stat`、`wc` 等。
   * **凭证读取防御**：操作数若指向 `.env`、`~/.ssh/id_*`、`.aws`、`.key` 等敏感凭证，坚决不放行。

2. **Plan 模式专用工具 (`tools.rs`, `provider.rs`)**：
   * 由 `PlanToolProvider` 向模型暴露：
     * `core__write_plan`：将实施方案原子持久化到 `<workspace>/.thunder/plans/<session_id>.md`。
     * `core__exit_plan_mode`：校验计划文件存在且非空，触发注册的 `ModeFlipCallback`，将宿主模式从 `plan` 翻转回 `yolo`（或 `ask`），并向桌面端推送 `ModeChanged` 事件。

3. **只读拦截中间件 (`middleware.rs`)**：
   * `PlanModeMiddleware`：实现 `thunder_core::ToolMiddleware`。
   * 在 Plan 模式下：
     * 显式放行 `write_plan` 与 `exit_plan_mode`（豁免写限制）；
     * 显式放行 `ask_user_question`（向人类提问澄清需求）；
     * 显式放行只读工具（`read_file`, `grep`, `find`, `ls`, `thunder__discover_tools`）；
     * 针对 `bash` 工具，调用 `read_only::call_is_read_only` 静态证明；
     * 对任何代码写工具（`write_file`, `edit_block`）和非只读脚本直接返回 `403` 拦截并引导模型输出方案。
