# 已知问题登记（Backlog）

> 用途：在推进 `docs/agent-loop-budget-and-recovery.md` 改造期间，把「顺路发现但不属于当前改动范围」的问题**原样留证**，避免边做边忘。
> 约定：每条记录现象、证据、复现方式、影响面、建议动作、状态。修复后不删除条目，把状态改为 `fixed` 并补上指向修复的说明。
>
> 回溯入口：本文件就是回溯清单。当前改造收尾时逐条过一遍。

状态标记：`open` / `in-progress` / `fixed` / `wontfix`

---

## KI-1 `.thunder/tools/` 热重载基本不工作（macOS）`open` `P0`

### 现象

文件监控启动后，写入 `.thunder/tools/*.sh` 不会触发 `CatalogSnapshot` 刷新。两个既有测试因此长期红：

| 测试 | 位置 | 断言 |
|---|---|---|
| `watcher_hot_reloads_new_script_into_arc_swap_snapshot` | `crates/thunder-registry/src/lib.rs:152` | 1.5s 内版本号前进且出现 `script__git_squash_feature` |
| `e2e_self_evolving_agent_hot_loads_script_with_100_percent_kv_cache_shield` | `crates/thunder-core/tests/e2e_microkernel.rs:111` | 2s 内快照更新 |

失败信息：`ArcSwap<CatalogSnapshot> must hot-reload new script without restart` / `Watcher must update CatalogSnapshot within 2s`。

### 证据

1. **不是本次改造引入的**：`git stash push --include-untracked` 清空工作区改动后，`cargo test -p thunder-core --test e2e_microkernel` 连跑 4 次，**4/4 全红**。
2. **不是注册竞态**：临时探针（已删除）在写入脚本前先 `sleep(500ms)` 让 watcher 稳定，仍然失败。
3. **同进程多 watcher 时 100% 失败**：两个探针测试跑在一个进程里，两条全红；单条单独跑 3 次是 1 过 2 挂。
4. 失败时 `tracing::info!` 的 `CatalogSnapshot hot-reloaded via ArcSwap` 一次都不打印，说明 `notify` 的回调从未到达 actor，而不是 `reload_scripts_dir` 出错。

### 可疑根因（未定论）

- `notify 6.1` 的 macOS FSEvents 后端 + `notify-debouncer-mini 0.4` 组合在同进程多实例时互相干扰。
- `crates/thunder-registry/src/watcher.rs:52` 把 `debouncer` move 进 `tokio::spawn` 的 async block，`let _keep_debouncer_alive = debouncer;` 只有在 future 第一次被 poll 时才绑定；在 current-thread runtime 下（`#[tokio::test]` 默认）这个窗口期内 watcher 的所有权状态值得怀疑。
- `RecursiveMode::NonRecursive` 在 FSEvents 后端语义与预期不符。

### 影响面

- 产品能力：脚本工具热挂载（`.thunder/tools/`）、`ThunderEvent::RegistryUpdated` 事件、daemon/TUI 的动态工具发现全部依赖它。
- CI：上面两个测试永久红/随机红，会掩盖真实回归。
- 与本次改造无关，但会让「测试全绿」这个验收条件失效。

### 建议动作

1. 先加一条**不依赖文件系统通知**的测试（直接调 `registry.reload_scripts_dir()`）保住 `reload` 路径的回归覆盖。
2. 热重载本身改为 **watch + 轮询兜底**（例如 1s 一次目录 mtime/digest 检查），不要单押 FSEvents。
3. 排查 `watcher.rs` 的 debouncer 所有权与生命周期，或改用 `notify` 的 `RecommendedWatcher` + 手动 debounce。

---

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

修复：周期检测由新模块 `crates/thunder-core/src/cycles.rs` 承担（Phase D）。**窗口错误率（抖动式错误）仍未做**，`consecutive_errors` 依旧被单次成功清零——见 KI-11。

---

## KI-6 `FinishReason::{MaxTurnsExceeded, BudgetExceeded}` 只有定义、零生产者 `fixed`

`crates/thunder-protocol/src/event.rs` 声明了这两个原因，全仓库没有任何代码产生它们。桌面端 `agent-resume-panel/apps/desktop/src/renderer-react/features/chat/useThunderChat.ts:219-232` 的 `failureNoticeForFinishReason()` **已经为这两个值写好了用户文案**，属于长期死代码。

修复：Phase D 接通，桌面端那两段文案不再是死代码。

---

## KI-7 面板「非 `done` 即无答案」契约与收尾轮冲突 `fixed`

`useThunderChat.ts:214-218` 的既有契约是「`loop_complete` 的 `finish_reason` 不是 `done`，就说明本次 run 没产出答案」。Phase D 引入收尾轮后会出现「有正文 + 非 `done` 原因」，面板会打出 `⚠️ Task stopped: ... without producing an answer`，与正文自相矛盾。

修复：§3.4 单一规则由 `AgentLoopOutcome::wire_finish_reason()` 实现，daemon 已改用；面板零改动。

---

## KI-8 e2e 测试依赖 workspace 构建顺序 `open` `P2`

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

## KI-11 连续错误计数仍被单次成功清零 `open` `P2`

`crates/thunder-core/src/loop_guard.rs` 的 `record_tool_result()` 一遇成功就把 `consecutive_errors` 归零，因此 `err,err,ok,err,err,ok` 这种抖动式失败永远不会触发熔断。原方案 §6.4 计划用「最近 12 次里错误 ≥ 8」的窗口错误率替代，Phase D 未实现（周期检测优先落地）。

建议：加滑动窗口错误率，保留现有连续计数作快通道。

---

## 附录：本次改造期间的验证命令

```bash
cargo clippy --workspace --all-targets
cargo test -p thunder-core --lib
cargo test -p thunder-core --test loop_guard_context_balance   # Phase A 回归
cargo test -p thunder-core --test progress_observation         # Phase B 观察
cargo test --workspace --no-fail-fast                          # 已知仅 KI-1 两条失败
```
