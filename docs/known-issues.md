# 已知问题登记（Backlog）

> 用途：在推进 `docs/agent-loop-budget-and-recovery.md` 改造期间，把「顺路发现但不属于当前改动范围」的问题**原样留证**，避免边做边忘。
> 约定：每条记录现象、证据、复现方式、影响面、建议动作、状态。修复后不删除条目，把状态改为 `fixed` 并补上指向修复的说明。
>
> 回溯入口：本文件就是回溯清单。当前改造收尾时逐条过一遍。

状态标记：`open` / `in-progress` / `fixed` / `wontfix`

---

## KI-1 `.thunder/tools/` 热重载基本不工作（macOS）`fixed` `P0`

### 现象

文件监控启动后，写入 `.thunder/tools/*.sh` 不会触发 `CatalogSnapshot` 刷新，两个既有测试长期红：

| 测试 | 位置 |
|---|---|
| `watcher_hot_reloads_new_script_into_arc_swap_snapshot` | `crates/thunder-registry/src/lib.rs` |
| `e2e_self_evolving_agent_hot_loads_script_with_100_percent_kv_cache_shield` | `crates/thunder-core/tests/e2e_microkernel.rs` |

### 根因（已定位）

`notify` 6.1 的 **macOS FSEvents 后端投递延迟无界且不可预测**。同一台机器连测三轮，写入后首个 `Modify(Data)` 事件的到达时间：

```
round 0: 910ms
round 1: 8424ms
round 2: 5427ms
```

而测试窗口是 1.5s / 2s，所以表现为「随机红」；延迟落在窗口内那次就是「偶尔绿」。用户侧感受就是「热重载坏了」。

排除了两个替代解释：

1. **不是本次改造引入的**：`git stash push --include-untracked` 清空改动后连跑 4 次，4/4 全红。
2. **不是注册竞态**：写入前先 `sleep(500ms)` 让 watcher 稳定，仍然失败；同进程双 watcher 时 100% 失败只是放大了延迟。

对照实验：同一目录、同一写入方式，`notify::poll::PollWatcher`（500ms 间隔 + `compare_contents`）**300ms 内稳定投递**。

### 修复

`crates/thunder-registry/src/watcher.rs` 改用 `PollWatcher`：

- `poll_interval` 默认 500ms（`DEFAULT_POLL_INTERVAL_MS`），延迟上界 ≤ 一个间隔，可预测。
- `with_compare_contents(true)`：按内容哈希比较，编辑器「写临时文件再 rename」的保存方式不会被漏掉。
- 去掉 `notify-debouncer-mini` 依赖：轮询本身每区间最多一批，debounce 只会再叠一层延迟。工作区依赖同步移除。
- 回调不再静默吞掉错误（`res.is_err()` 原先是空操作），改为 `warn!`。
- 新增 `start_hot_reload_watcher_with_interval`，并把早已存在但从未生效的 `sandbox.debounce_ms` 配置真正接上（daemon / TUI / CLI 三个入口），默认值改为 500。

### 验证

```
cargo test -p thunder-registry --lib       → 5 连跑全绿，0.52s（原 1.58s 且随机红）
cargo test -p thunder-core --test e2e_microkernel → 5 连跑全绿，0.66s
cargo test --workspace --no-fail-fast      → 57 个 suite 全绿
```

代价：轮询有固定 IO（每个间隔读一遍 tools 目录的文件内容）。该目录常态只有几个小脚本，可接受；若将来承载大量脚本，应改回事件驱动并给 FSEvents 延迟加上界。

## KI-2 `max_turns` 撞限直接返回 `Err`，产出丢失 `fixed`

`crates/thunder-core/src/agent_loop.rs` 的 `for turn in 1..=max_turns` 跑完即 `Err("AgentLoop exceeded maximum turns")`，不收尾轮、无 `final_response`、不发 `LoopComplete`。

修复：Phase D 收尾轮 + `finish_reason` 贯穿（见方案 §7.3）。

---

## KI-3 早退路径留下未配对的 `tool_call` `fixed`

`agent_loop.rs` 的硬重复 / 熔断 / 批内 cancel 三条早退路径不补 tool 消息，assistant 消息留下孤儿 `tool_calls`；`providers/client.rs:270-315` 无配对修复，下一轮直接 400，daemon 存盘后该会话永久坏死。

修复（Phase A）：`close_pending_tool_calls()` + `context::repair_tool_call_pairing()`（装载期自愈旧数据）+ `tests/loop_guard_context_balance.rs` 3 条回归。

---

## KI-4 core 用工具名嗅探「是不是写工具」 `open` `P2`

`crates/thunder-core/src/pruning/checkpoint.rs:249` 的 `is_mutating_tool_name()` 靠名字里是否含 `write/edit/delete/remove/patch/update/create` 来判断工具会不会改文件，违反微内核边界，对第三方 MCP 与脚本工具全部失效。

`ProgressKind`（Phase B 已落地）提供了正解，但 `extract_file_lists(messages)` 目前只拿得到消息、拿不到 registry 快照，需要把谓词一路传进 `ContextPruner::prune`。属于重构，未纳入当前 PR。

---

## KI-5 `LoopGuard` 的两处检测盲区 `fixed`

1. `A-B-A-B` 周期循环全盲——只判「连续 N 条完全相等」（`crates/thunder-core/src/loop_guard.rs`）。
2. 连续错误计数被**单次成功清零**，`err,err,ok,err,err,ok` 永不触发熔断。

修复：周期检测由新模块 `crates/thunder-core/src/cycles.rs` 承担（Phase D）；抖动式错误由 KI-11 的滑动窗口错误率覆盖。

---

## KI-6 `FinishReason::{MaxTurnsExceeded, BudgetExceeded}` 只有定义、零生产者 `fixed`

`crates/thunder-protocol/src/event.rs` 声明了这两个原因，全仓库没有任何代码产生它们。桌面端 `agent-resume-panel/apps/desktop/src/renderer-react/features/chat/useThunderChat.ts:219-232` 的 `failureNoticeForFinishReason()` **已经为这两个值写好了用户文案**，属于长期死代码。

修复：Phase D 接通，桌面端那两段文案不再是死代码。

---

## KI-7 面板「非 `done` 即无答案」契约与收尾轮冲突 `fixed`

`useThunderChat.ts:214-218` 的既有契约是「`loop_complete` 的 `finish_reason` 不是 `done`，就说明本次 run 没产出答案」。Phase D 引入收尾轮后会出现「有正文 + 非 `done` 原因」，面板会打出 `⚠️ Task stopped: ... without producing an answer`，与正文自相矛盾。

修复：§3.4 单一规则由 `AgentLoopOutcome::wire_finish_reason()` 实现，daemon 已改用；面板零改动。

---

## KI-8 e2e 测试依赖 workspace 构建顺序 `open` `P3`

`crates/thunder-core/tests/e2e_microkernel.rs:16` 的 `find_or_build_thunder_std()` 直接 panic 要求 `target/debug/thunder-std` 已存在。因此 `cargo test -p thunder-core` 在干净 checkout 上会因为「只编译了 thunder-core 及其依赖」而直接失败，必须 `cargo test --workspace` 或先 `cargo build --workspace`。

建议：改成在测试里按需 `cargo build -p thunder-std`（或至少给出可操作的 panic 文案与自动重试）。

---

## KI-9 `bash` 无法参与进度判定，只能靠预算兜底 `wontfix`（记录为设计取舍）

`bash` 的副作用无法由参数刻画（`echo x > f`），因此必须声明 `ProgressKind::Neutral`，只参与输出指纹判定；输出里带耗时/PID 时指纹永远「新鲜」，停滞检测对它失效，只能靠 `wall_clock` 与 `max_turns` 兜底。

彻底解法需要插件把结构化副作用回传（`ToolOutcome { text, progress }`），见方案 §13 Phase F。当前不修。

---

## KI-10 Phase C 期间存在两个 `max_turns` 旋钮 `fixed`

`AgentLoopConfig.max_turns`（真正约束 `for turn in 1..=max_turns`）与 `LoopBudget.max_turns`（Phase C 仅观察）暂时并存，含义重叠且容易配错（改一个另一个不生效）。

修复：Phase D 删除 `AgentLoopConfig.max_turns`，循环上界取自 `budget.max_turns`。

---

## KI-12 L2 整轮重启只有开关没有实现 `open` `P3`

`BudgetState::allow_restart()` 与 `AgentLoopConfig.budget.max_restarts` 已就位，但循环里没有任何调用点：Phase E 只实现了 L1（上下文重置）。要在 L2 生效，除了调用点还需要「每次重启必须改变一个变量」的机制（换模型 / 收敛工具集 / 挂 plan 中间件），否则等于确定性复读同一个死循环。

---

## KI-11 连续错误计数仍被单次成功清零 `fixed` `P2`

`crates/thunder-core/src/loop_guard.rs` 的 `record_tool_result()` 一遇成功就把 `consecutive_errors` 归零，因此 `err,err,ok,err,err,ok` 这种抖动式失败永远不会触发熔断。原方案 §6.4 计划用「最近 12 次里错误 ≥ 8」的窗口错误率替代，Phase D 未实现（周期检测优先落地）。

修复：`LoopGuard` 增加滑动窗口错误率（默认「最近 12 次里 ≥ 8 次失败」），与连续计数并行：连续计数当快通道，窗口负责 `err,err,ok,err,err,ok` 这类抖动。中止消息同时报告「连续 N 次」与「最近 W 次里 M 次失败」。`with_error_window()` 可覆盖阈值。

---

## KI-13 CLI 的 LLM 请求超时被错接成 sandbox 超时 `fixed` `P1`

`crates/thunder-core/src/main.rs` 用 `client_for_selection(&config.agent.model, config.sandbox.timeout_ms)` 构造模型客户端，即 **LLM HTTP/SSE 超时 = 沙箱工具超时（默认 30s）**。真实任务里一次较长的生成就会在流中途被打断：

```
Thunder Agent error: sse error: error reading sse body: error decoding response body
(caused by: request or response body error; operation timed out)
```

三个入口互相不一致：daemon `REQUEST_TIMEOUT_MS = 120_000`、TUI 硬编码 `120_000`、CLI 用 sandbox 的 30s。

修复：CLI 改用独立的 `LLM_REQUEST_TIMEOUT_MS = 120_000`。真机验证的第二轮任务因此跑通（136s / 15 次调用）。

遗留：三个入口各自硬编码 120s，应提到配置（如 `agent.request_timeout_secs`）统一管理。

---

## 附录：本次改造期间的验证命令

```bash
cargo clippy --workspace --all-targets
cargo test -p thunder-core --lib
cargo test -p thunder-core --test loop_guard_context_balance   # Phase A 回归
cargo test -p thunder-core --test progress_observation         # Phase B 观察
cargo test --workspace --no-fail-fast                          # 57 个 suite 全绿
```
