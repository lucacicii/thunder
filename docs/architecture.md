# Thunder 架构详解

> 版本：0.6.0（`Cargo.toml:17`）｜上一版 0.5.0 的发版说明见 Git Tag `v0.5.0`，0.4.0 的迁移说明见 `docs/migration-0.4.md`
> 本文是**代码事实文档**：所有结论都对应到具体 `文件:行`。与 `docs/permission-structure.md`（权限专项）和 `docs/migration-0.4.md`（迁移说明）互补。
>
> ⚠️ **行号会漂**：文中引用以基线 `171694d` 为准。任何一次改动都可能让某个区间整体平移（例如 `loop_engine/engine.rs` 在第 17 步之后曾整体 +12 行，`mcp/src/stdio.rs` 整段 +85 行）。**引用行号前先 grep 一次**，不要把它当稳定坐标；反过来，如果发现某处行号对不上，先怀疑行号，再怀疑结论。

---

## 0. 一句话架构

Thunder 是**一个单 Agent 闭环内核 + 一个微内核宿主 + 若干可插拔能力包**组成的 Rust Cargo Workspace：

```
┌────────────────────────────────────────────────────────────────────────┐
│  宿主层（Host / B 侧）                                                  │
│  thunder-tui（交互终端）   thunder-agent-daemon（STDIO sidecar）        │
│  thunder-agent-runtime（JSONL 无头运行时）                              │
└───────────────┬────────────────────────────────────────────────────────┘
                │ 只通过 start / join / cancel / events 驱动
┌───────────────▼────────────────────────────────────────────────────────┐
│  thunder-agent-root —— 微内核宿主：插件选择 → 装配 → 起跑              │
└───────────────┬────────────────────────────────────────────────────────┘
                │ AgentLoop::new().with_*().start()
┌───────────────▼────────────────────────────────────────────────────────┐
│  thunder-agent-loop —— Agent A：单任务原子闭环                          │
│  LLM 流式推理 → 工具并发执行 → 上下文压缩 → 循环守卫 → LoopComplete     │
└───────────────┬────────────────────────────────────────────────────────┘
                │ 三个具名接缝（全部由宿主注入）
   ┌────────────┼────────────┬─────────────────┬──────────────────┐
   ▼            ▼            ▼                 ▼                  ▼
LLMClientTrait  ToolScope   ToolGate      ToolTraceSink     HostUi
（providers）   （pack-code）（root/mode）  （root/daemon）   （tui/daemon）
```

核心不变量：**内核不知道什么是"工作区"、什么是"用户可以接受的风险"**。前者由 `ToolScope` 注入，后者由 `ToolGate` 注入（`types/scope.rs:1-18`、`types/gate.rs:1-22`）。

> 版本说明：`thunder-agent-mcp` 的 stdio 传输曾有一个**阻断级缺陷**（server 在 spawn 返回瞬间被 SIGKILL），已在本次修改中修复并添加真实子进程回归测试——见 [§15.2.1](#1521-已修复stdio-server-曾被瞬间杀死)；仍未修的项见 [§18.5](#185-已知功能缺陷)。

---

## 1. 工作区总览

### 1.1 包清单与体量

| Crate | 目录 | 源文件 | 源码行数 | 角色 |
|---|---|---|---|---|
| `thunder-agent-types` | `thunder-agent-types` | 9 | 1,480 | 纯 wire：消息 / 事件 / 错误 / 工具定义与结果 / policy 声明 / scope / trace 记录 |
| `thunder-agent-policy` | `thunder-agent-policy` | 13 | 4,525 | 策略层：`AgentTool` / `ToolExecutor`（唯一漏斗）/ `ToolRegistry` / gate / ui / interceptor / read_only |
| `thunder-agent-loop` | `thunder-agent-loop` | 31 | 6,331 | Agent 内核（A）：loop / context / pruning / stream / cache / seam / scratchpad |
| `thunder-agent-pack-code` | `thunder-agent-pack-code` | 11 | 2,764 | 编码能力包：fs / shell / search + `CodeScope` |
| `thunder-agent-providers` | `thunder-agent-providers` | 13 | 3,286 | 模型目录 + 进程内流式 HTTP |
| `thunder-agent-runtime` | `thunder-agent-runtime` | 4 | 1,072 | JSONL 无头运行时 |
| `thunder-agent-skills` | `thunder-agent-skills` | 7 | 1,318 | Skill 解析与注册 |
| `thunder-agent-mcp` | `thunder-agent-mcp` | 11 | 2,101 | MCP 客户端 |
| `thunder-agent-root` | `thunder-agent-root` | 26 | 10,857 | 宿主装配（`Assembly`）+ 内置 sections/packs/observers |
| `thunder-agent-daemon` | `thunder-agent-daemon` | 7 | 4,268 | STDIO sidecar |
| `thunder-conversation` | `thunder-agent-core/conversation` | 11 | 2,050 | 会话存储 |
| `thunder-tui` | `thunder-agent-core/tui` | 25 | 14,284 | 终端 UI |

口径：各 crate `src/` 下的 `.rs` 文件数与行数（**不含** `tests/`、`examples/`，统计于 `8ebbe5e`）。工作区合计 **54,336 行**（`Cargo.toml` 定义 12 个成员）。

> 这张表是**快照**，每改一次代码就过期一分：当量级参考，不要当事实引用。上一版混用了「src only」与「含 tests/examples」两种口径，数字互相矛盾（合计写 58,400，两种口径实测分别是 45,352 与 62,373）。

### 1.2 依赖方向（硬约束）

各 crate 直接依赖的**内部** crate（逐条抄自各自的 `Cargo.toml`；`loop` 一栏为空是硬约束，不是遗漏）：

| Crate | 依赖的内部 crate |
|---|---|
| `thunder-agent-types` | —（纯 wire） |
| `thunder-agent-loop` | `types`（`policy` 只在 `[dev-dependencies]`，用于测试装配） |
| `thunder-agent-policy` | `types`、`loop`（`registry_tools` 实现 loop 的 `Tools`） |
| `thunder-agent-pack-code` | `types`、`policy`、`loop` |
| `thunder-agent-providers` | `types`、`policy`、`loop` |
| `thunder-agent-skills` | `types`、`policy`、`loop` |
| `thunder-agent-mcp` | `types`、`policy`、`loop` |
| `thunder-conversation` | `loop` |
| `thunder-agent-runtime` | `types`、`policy`、`loop`、`providers` |
| `thunder-agent-root` | `types`、`policy`、`loop`、`pack-code`、`providers` |
| `thunder-agent-daemon` | `types`、`policy`、`loop`、`providers`、`skills`、`mcp`、`conversation`、`root` |
| `thunder-tui` | `types`、`policy`、`loop`、`pack-code`、`providers`、`skills`、`mcp`、`conversation`、`root` |

> 内核不依赖任何宿主仍是硬约束；它依赖 `types`，而 `types` 只含 wire 类型。`policy → loop` 的方向是"策略层实现内核的 `Tools` 接缝"，不是内核反向依赖策略。

> 上一版这里是一张手绘箭头图，它把 `root` / `daemon` / `tui` 画成只依赖 `loop`（和 `root`），漏掉了 `pack-code`、`providers`、`skills`、`mcp`、`conversation` 这几条真实存在的边。

- 全部使用 **path 依赖**，`[workspace.dependencies]` 统一版本（`Cargo.toml:30-67`）。
- `thunder-agent-loop` **不依赖任何宿主**；`ARCHITECTURE.md:90-97` 把它写成契约："A never imports B"。
- `thunder-agent-root` 的 `conversation` / `skills` / `mcp` 是**可选特性**，默认全开（`thunder-agent-root/Cargo.toml` 的 `[features]`）。

### 1.3 构建与门禁

| 项 | 值 | 来源 |
|---|---|---|
| Rust | 1.80+（workspace 最低）／构建与 lint 用 **1.98.0**（`rust-toolchain.toml` 与 CI 同步 pin） | `Cargo.toml:19`、`rust-toolchain.toml:11-13`、`.github/workflows/ci.yml` |
| `unsafe_code` | **deny**（全工作区） | `Cargo.toml:72-73` |
| clippy | `all = warn`，`too_many_arguments` / `type_complexity` / `result_large_err` allow | `Cargo.toml:75-80` |
| release profile | `opt-level=3` + `lto="fat"` + `codegen-units=1` + `strip` | `Cargo.toml:82-86` |
| `tui-dev` profile | 继承 release，关 LTO、16 CGU、incremental（TUI 迭代用） | `Cargo.toml:93-99` |
| 版本一致性门禁 | `python3 scripts/check-version.py`：单一 workspace 版本 + 每个成员 `version.workspace = true`，`ci.yml` 与 `release.yml` 共用同一份实现 | `scripts/check-version.py`、`.github/workflows/ci.yml` |
| CI 步骤 | lint job：`fmt --check` → `clippy -D warnings` → `check-version.py`；test job：setup Node 22 → `cargo check` → `./test.sh` | `.github/workflows/ci.yml` |
| 测试特性 | 一律带 `--features thunder-agent-daemon/testing-mock` | `test.sh`、`ci.yml` |

脚本入口：`./run.sh`（TUI，默认 release）、`./daemon.sh`（daemon，debug）、`./test.sh`（`cargo test --workspace --no-fail-fast`）。

---

## 2. 内核：`thunder-agent-loop`

### 2.1 模块地图

| 文件 | 行 | 职责 |
|---|---|---|
| `src/lib.rs` | 71 | `prelude` 导出全部稳定 API |
| `loop_engine/engine.rs` | 1721 | `AgentLoop`：装配、`start`/`run`、主循环、结算 |
| `loop_engine/handle.rs` | 151 | `AgentHandle`：join / cancel / events / status |
| `loop_engine/gate.rs` | 69 | `CompletionGate`：完成判定接缝 |
| `loop_engine/guard.rs` | 158 | `LoopGuard`：重复动作 / 连续错误熔断 |
| `loop_engine/hooks.rs` | 31 | `AgentEventDispatcher`：broadcast 旁路 |
| `tools/executor.rs` | 930 | `ToolExecutor`：**唯一工具执行入口** + trace 闭环 |
| `tools/registry.rs` | 214 | `ToolRegistry`：BTreeMap 注册表、超时、截断、scratchpad |
| `tools/guard.rs` | 437 | `Guard`：内核地板（危险命令 / 关键路径） |
| `tools/trace.rs` | 595 | `ToolTrace` 模型 + 脱敏 |
| `tools/scratchpad.rs` | 277 | 超大输出落盘 + manifest |
| `tools/sanitizer.rs` | 124 | ANSI / 二进制污染清理 |
| `tools/notice.rs` | 93 | `SystemNotice`：面向模型的遥测块 |
| `pruning/strategy.rs` | 193 | `ContextPruner`：检查点压缩 + 紧急裁剪 |
| `pruning/checkpoint.rs` | 614 | 摘要提示词、切点选择、`Summarizer` |
| `pruning/error_detector.rs` | 133 | 从供应商报错里解析真实窗口上限 |
| `cache/warmer.rs` | 396 | Prompt Cache 保温 |
| `core/context.rs` | 197 | `ContextBuffer`：带 token 记账的消息向量 |
| `core/steer.rs` | 286 | `SteerQueues` / `PendingQueue` |
| `core/pause.rs` | 110 | `PauseGate` |
| `core/state.rs` | 188 | `AgentStateTracker` / `LoopStatus` |
| `core/paths.rs` | 152 | 用户数据根解析（`THUNDER_CONFIG_DIR`） |
| `core/token_estimator.rs` | 81 | 零分配 token 估算 |
| `core/images.rs` | 184 | 图片 magic-byte 校验 + base64 |
| `core/utf8.rs` | 60 | 安全切片 / 截断 |
| `types/*` | 2,215 | 配置、事件、消息、工具、gate、scope、policy、ui、invoke |

### 2.2 装配：`AgentLoop`

```rust
pub struct AgentLoop {
    id: String,
    config: AgentConfig,
    llm_client: Arc<dyn LLMClientTrait>,        // 默认 UnconfiguredLLMClient
    tool_registry: ToolRegistry,                 // BTreeMap，顺序稳定
    tool_executor: ToolExecutor,                 // 注册表快照 + 四个接缝
    event_dispatcher: AgentEventDispatcher,      // broadcast(128)，有损旁路
    scratchpad: ScratchpadManager,
    running: Arc<AtomicBool>,                    // 单任务闸门
    status: Arc<AtomicU8>,                       // LoopStatus
    pause_gate: Arc<PauseGate>,
    steer_queues: Arc<SteerQueues>,
    tool_gate: Option<Arc<dyn ToolGate>>,        // None = 除守卫外全放行
    host_ui: Option<Arc<dyn HostUi>>,            // None = 一切 Ask 拒绝
    completion_gate: Option<Arc<dyn CompletionGate>>,
    max_gate_rounds: usize,                      // 默认 3
    trace_sink: Option<Arc<dyn ToolTraceSink>>,
    tool_scope: Option<Arc<dyn ToolScope>>,      // None = 一切 Unknown
}
```
（`loop_engine/engine.rs:47-94`）

建造器方法：`with_id` / `with_custom_client` / `register_tool` / `with_tool_gate(gate, ui)` / `with_tool_scope` / `with_trace_sink` / `with_completion_gate(gate, max_rounds)` / `with_pause_gate` / `with_steer_queues` / `with_tool_executor`。

**`refresh_executor()`（`engine.rs:199-220`）是装配的关键**：executor 持有注册表快照，任何"后注册"都必须重建，否则新工具/新 gate 不生效。`register_tool`、`with_tool_gate`、`with_tool_scope`、`with_trace_sink`、`with_id` 都调用它。

### 2.3 生命周期契约

| 规则 | 实现 |
|---|---|
| 一个 `AgentLoop` 同时只能跑一个任务 | `running.swap(true, SeqCst)` → `AgentError::AlreadyRunning`（`engine.rs:309-313`） |
| 并行 = 再 `new` 一个实例 | `tests/unit_contract_test.rs:95` |
| 事件带 `agent_id` 以便解复用 | `ObservedEvent`（`types/event.rs:52-60`） |
| 事件流可靠（背压、丢弃可观测） | 每 run 一条 `mpsc::channel(512)`（`engine.rs`）；micro-delta 满时 `try_send` 计数，结构化事件 5 秒 deadline 未送达计数 + `warn!`，`AgentLoop::event_drop_counters()` 可读 |
| `run()` = `start` + 排空事件 + `join` | `engine.rs:337-347` |
| 释放句柄**不**取消任务 | `handle.rs:14-15` 注释；须显式 `cancel()` |
| panic 也要清忙标志 | `RunningGuard::drop`（`handle.rs:137-151`） |
| `config.system_prompt` 是该 run 的唯一事实来源 | 三种 `ContextInput` 形态（`Text` / `Messages` / `Buffer`）在 `spawn_loop` 里统一经 `ContextBuffer::set_system_prompt` 归一：转录里的旧 system 消息只被替换，不会压过宿主本次装配的提示（`engine.rs`）；无宿主提示时才保留转录自带的那条 |

### 2.4 主循环（`spawn_loop` → `tokio::spawn`）

一轮（turn）的完整序列，按代码顺序：

```
loop {
  1. max_turns 检查            → FinishReason::MaxTurnsExceeded        engine.rs:471
  2. cancel_token 检查          → FinishReason::Cancelled               engine.rs:479
  3. LoopGuard::is_circuit_broken → FinishReason::Error（连续工具失败）  engine.rs:485
  4. LoopGuard::is_repetition_limit_exceeded → Error（重复调用）        engine.rs:502
  5. 快照 raw_log（首次压缩前）                                         engine.rs:518
  6. pruner.prune(...)          → 事件 ContextCompacted                 engine.rs:521
  7. max_tokens_budget 检查      → FinishReason::BudgetExceeded          engine.rs:545
  8. tracker.next_turn() + 事件 TurnStart                                engine.rs:558
  9. 排空 steer 队列 → push 进上下文 + 事件 SteerAccepted（follow_up 只在第 17 步的无工具分支排空，两者不同时）  engine.rs:582-613
 10. ┌ 内层流式循环 ───────────────────────────────────────────────┐
     │ llm_client.stream_chat(ChatRequestOptions)                    │
     │   ├ Err + 上下文溢出 + retry<2 → update_max_tokens + 压缩 + 重试 │
     │   ├ Err + 网络握手失败 + retry<max_stream_retries → 指数退避重试 │
     │   └ Err 其它 → 事件 Error → break                             │
     │ 消费 LLMStreamChunk：                                          │
     │   Token → TokenDelta；ReasoningToken → ReasoningDelta          │
     │   ToolCallChunk → ToolCallChunk（标记 had_tool_chunks）        │
     │   Completed{...} → 暂存 completed_chunk                        │
     │   Err → 溢出/中断分类                                           │
     └───────────────────────────────────────────────────────────────┘
 11. 流中断三分支恢复（Scenario A/B/C）                                 engine.rs:790-854
 12. 注入的续写提示词从上下文尾部弹出，保持历史洁净                     engine.rs:860-862
 13. 取消 → 保留 partial answer → Cancelled                            engine.rs:864
 14. token 记账 + 回退估算（completion/prompt/reasoning/tps）           engine.rs:920-1005
 15. Prompt Cache 保温调度（每轮替换上一代）                            engine.rs:981-1005
 16. push assistant 消息 + 事件 TurnEnd                                 engine.rs:1020-1050
 17. 无工具调用 → 队列 → CompletionGate → Done / retry / fail            engine.rs:1052-1176
 18. 有工具调用 → 状态 ExecutingTools                                   engine.rs:1178
 19. 逐个发 ToolCallReady / ToolExecStart                              engine.rs:1181-1204
 20. PauseGate 检查（工具边界，绝不打断在途工具）                       engine.rs:1208-1220
 21. ToolExecutor::execute_all(...)  ← 并发 join_all                   engine.rs:1222
 22. 逐结果：统计 / 熔断记账 / 重复告警 / 截断诊断 → 事件 ToolExecResult │
     + TelemetryNotice → push tool 消息                                 engine.rs:1232-1300
 23. 状态回到 Running，下一轮
}

结算（settle）：
  A. 非 Done 时修补悬空 tool_call（dangling_tool_calls）→ 补 CANCELLED_TOOL_RESULT  engine.rs:1314-1348
  B. Cancelled 时把 partial answer 写回 transcript                                engine.rs:1350-1361
  C. 终态 LoopStatus::Completed/Aborted/Failed                                     engine.rs:1364-1370
  D. clear_run_trace()                                                             engine.rs:1374
  E. cache::warmer::mark_idle(session_id)                                          engine.rs:1378
  F. scratchpad.cleanup()（仅 auto_cleanup）                                        engine.rs:1393
  G. 事件 LoopComplete + oneshot 回传 AgentRunResult                               engine.rs:1399-1418
```

### 2.5 `AgentRunResult`：宿主拿到的产物

```rust
pub struct AgentRunResult {
    pub agent_id: String,
    pub final_content: Option<String>,
    pub messages: Vec<ChatMessage>,        // LLM 实际看到的投影（含检查点摘要）
    pub raw_messages: Option<Vec<ChatMessage>>, // 仅发生压缩时：压缩前原始轨迹
    pub stats: AgentStats,
    pub finish_reason: FinishReason,
}
```
（`engine.rs:26-40`）

`raw_messages` 的存在是显式设计："原始历史从不被静默销毁"（`engine.rs:34-37`）。宿主持久化 `messages` 即可续跑（`ARCHITECTURE.md:30-31`）。

### 2.6 `FinishReason`

| 值 | 触发 |
|---|---|
| `Done` | 模型不再调用工具且队列空、gate 通过 |
| `MaxTurnsExceeded` | `config.max_turns`（默认 50） |
| `BudgetExceeded` | `config.max_tokens_budget` |
| `Cancelled` | 取消令牌 |
| `Error` | 熔断、重复上限、流错误、gate `Fail` |
（`types/event.rs:41-49`）

---

## 3. 工具执行：单一漏斗

### 3.1 唯一入口 `ToolExecutor::run_tool`

`run_tool`（`tools/executor.rs:249-281`）**没有任何 early return**：全部判定搬进私有 `dispatch`，由它填 `&mut TraceDraft`。于是"每次调用必然留下且只留下一条 trace"是**结构性的**，不是约定（`executor.rs:241-248`）。

`dispatch` 的判定顺序（`executor.rs:287-389`）：

| # | 步骤 | 代码 | 结果 |
|---|---|---|---|
| 1 | **取消** | `:296` | `Decision::Cancelled` → `cancelled_refusal` |
| 2 | 参数解析 + **脱敏** | `:301-309` | 畸形参数 → `Value::Null`，照常走完 |
| 3 | **作用域** `ToolScope::scope_of` | `:311` | 无 scope → `Scope::Unknown` |
| 4 | **守卫** `Guard::check` | `:314` | `Decision::Deny{by: Guard}` → `guard_refusal` |
| 5 | **判定** `ToolGate::decide` | `:331-341` | 无 gate → `Allow` |
| 6 | **弹窗**（gate 返回 `Ask`） | `:371` → `ask()` | 超时/无面板/取消/非法答案 → 拒绝 |
| 7 | **执行与洋葱流水线** | `:424-508` | `execute_tool`: pre 降序拦截 → 注册表执行 → post 升序解套与结果同步 |
| 8 | **关 trace + 发布** | `run_tool:302-303` | 交给 `ToolTraceSink` |

内核**自己不落盘 trace**：它不知道 store 在哪（`executor.rs:516-520`）。

### 3.2 守卫：内核地板

`Guard::check`（`tools/guard.rs:61-87`）只拒两类：

1. **宿主声明 `Scope::Critical`** → `GuardHit{kind: CriticalPath, pattern:"critical path"}`；
2. **shell 类工具 + 命令命中危险集合** → `GuardHit{kind: DangerousCommand}`。

shell 类判定（`guard.rs:97-106`）：`effect == Exec`，或名字是 `bash|shell|sh|zsh|powershell`。

危险集合**按 token 判定**（`guard.rs:125-180`）：

| 规则 | 判定 |
|---|---|
| `mkfs*` | 首动词以 `mkfs` 开头 |
| `dd if=` | 动词 `dd` 且某 operand 以 `if=` 开头 |
| `rm -rf /` | 动词 `rm` **且**含 `r/R` flag **且**含 `f` flag **且**某 operand 是文件系统根 |
| fork bomb | 去空白后含 `:(){:|:&};:`（唯一保留的文本匹配，`guard.rs:182-191`） |

分词器 `segments`（`guard.rs:200-251`）跟踪引号与转义；`; | & ( ) { } \` > <` 与换行都是简单命令边界；`WRAPPERS`（`sudo doas env command exec nohup nice ionice time stdbuf xargs`，`guard.rs:35-37`）与 `VAR=value` 赋值被跳过。

设计理由写在文件头（`guard.rs:9-27`）：**名单故意很小，绝不靠猜测增长**；匹配 token 而非子串，因为旧实现让 `rm -rf /` 误伤 `rm -rf /tmp/build` 与 `git commit -m "never run rm -rf /"`；**无法解析的一律不在这里拒**，而是升级为 gate 的"问"。

### 3.3 作用域：位置，不是裁决

```rust
pub enum Scope { InScope, OutOfScope, Critical, #[default] Unknown }
pub trait ToolScope: Send + Sync {
    fn scope_of(&self, tool: &str, effect: ToolEffect, arguments: &serde_json::Value) -> Scope;
}
```
（`types/scope.rs:25-73`）

不对称性是设计核心（`types/scope.rs:8-12`）：**作用域只能把 `ask` 放宽成 `allow`，永远不能把 `allow` 收紧成 `deny`**——新增路径启发式最多让人多被问一次，不会造成静默失败。唯一例外是 `Critical`（宿主的"禁区"声明，`scope.rs:31-38`）。

`scope_of` 刻意**不是 async**：它是对内核已持有数据的纯分类（`scope.rs:64-66`）。

### 3.4 Gate：内核唯一的策略钩子

```rust
pub trait ToolGate: Send + Sync {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict;   // 每次调用都问，从不缓存
    fn record_answer(&self, request: &ToolGateRequest<'_>, answer: &ApprovalAnswer);
    fn label(&self) -> &str;                                          // 写进 trace 的 mode 字段
}
pub enum GateVerdict { Allow, AllowByRule{rule}, Ask(ApprovalPrompt), Deny{reason} }
```
（`types/gate.rs:52-142`）

两条内核保证（`gate.rs:9-22`）：守卫先跑且不咨询 gate；**弹窗是内核的**——gate 只能说"问"，串行化/超时/无面板处理全在 `ask()` 里。所以 gate 无法把"ask"实现成"allow"，也无法提供内核不认识的脚枪选项。

审批对话框的词汇表也归内核（`gate.rs:144-152`）：`Allow once` / `Always allow` / `Deny` / `Deny with reason`。`Always allow` 仅在 `prompt.allow_always` 为真时提供（`executor.rs:482-495`）。

### 3.5 弹窗：fail-closed 闭环

`ask()`（`executor.rs:432-587`）：

- 串行化：`prompt_lock: Arc<tokio::sync::Mutex<()>>`，与取消令牌 `tokio::select!` 竞速（`:444-455`）；
- 无面板 → `AskAnswer::NoUi` + 拒绝（`:460-467`）；
- 插件调用 → 标题变为 `Plugin <id> requests: ...`（`:473-476`）；
- 答案映射（`:522-582`）：`Allow once`→执行；`Allow always`→`gate.record_answer(AllowAlways)` + 执行；`Deny`→拒绝；`Deny with reason`→再弹 `Input` 追问，原因原文回灌模型；**无答案/集合外答案一律拒绝**；
- 默认超时 **600 秒**（`DEFAULT_APPROVAL_TIMEOUT`，`executor.rs:31`）。

`AskAnswer` 六态（`trace.rs:154-170`）：`allow_once` / `allow_always` / `denied` / `timeout` / `no_ui` / `unrecognised`，**只有前两个是真实的人类批准**。

### 3.6 并发与超时

```rust
pub async fn execute_all(&self, tool_calls: &[ToolCall], turn: usize,
                         cancellation_token: CancellationToken,
                         route: Option<String>, custom_timeout: Option<Duration>)
    -> Vec<ExecutedToolResult>
```
（`executor.rs:610-649`）——`futures_util::join_all` 并发派发，每个都走 `run_tool`，所以并行调用同样被盖章、判定、记录。

超时在 `ToolRegistry::execute_tool_call`（`registry.rs:123-135`）：`tokio::time::timeout(custom_timeout.unwrap_or(default_timeout), tool.execute(...))`，与取消令牌 `select!` 竞速；默认 30s（`registry.rs:24`），内核调用点传 `config.request_timeout_ms`（默认 60s）。

### 3.7 输出后处理链

```
tool.execute → Ok(output)
   → sanitize_tool_output：ANSI 剥离 + 二进制污染检测（>8% 控制字符或含 NUL → 整段替换）   sanitizer.rs:81-88
   → 若 > scratchpad.threshold_bytes(16KB) → 落盘 + 返回句柄消息（truncated=true）        registry.rs:144-165
   → 否则 format_and_truncate：head/tail 截断到 max_output_bytes(64KB)                     registry.rs:176-213
```
`BTreeMap` 而非 `HashMap` 存工具（`registry.rs:12-16`）：工具定义每轮序列化进请求，顺序稳定是 **prompt cache 命中的硬前提**。

### 3.8 trace 模型

```rust
pub const TRACE_SCHEMA: u8 = 1;
pub struct ToolTrace {
    schema, trace_id, session_id, task_id, turn, tool_call_id, tool, caller,
    effect, mode, scope, arguments /*脱敏后*/, decision, guard, answer,
    result: TraceResult { is_error, truncated, original_bytes, result_bytes,
                          output_preview /*2048 字符*/, output_digest },
    redactions: Vec<String>, timing: TraceTiming, started_at_ms, finished_at_ms,
}
pub enum Decision { Allow, AllowByRule{rule}, Asked, Deny{reason, by: DenyBy}, Cancelled }
pub enum DenyBy { Guard, Gate }
```
（`tools/trace.rs:28-215`）

- 记录的是**判定本身**（谁拒的、什么作用域、用户答了什么、答了多久）；
- 脱敏在入口做：密钥键名 + 密钥形态文本 + 文档正文摘要化（`executor.rs:307-309`）；
- `output_digest` 是完整**未脱敏**输出的稳定摘要，足以判断两次调用是否同产物而不留字节（`trace.rs:187-189`）。

两个出口共用一条记录（`engine.rs:1519-1545`）：`AgentEvent::ToolTrace` 给排事件的宿主，`ToolTraceSink` 给不想解析事件的宿主；**内核只序列化、只脱敏一次**。

### 3.9 拦截器洋葱流水线、递归调用防死锁与 MCP 流式进度

`thunder-agent-loop` 内置基于 MCP 规范标准对齐的插件与拦截器流水线（`types/plugin.rs`、`tools/executor.rs`）：

1. **优先级分层契约（`InterceptorPlugin::priority`）**：
   - `priority(&self) -> i32` 规范了拦截器在洋葱模型中的相对层次：
     - `CRITICAL_GUARD (1000)`：系统底层硬安全防御（危险命令、破坏性操作熔断）
     - `SECURITY_POLICY (800)`：工作区路径与权限安全边界（`CodeScope`）
     - `GOVERNANCE_MODE (500)`：人机协作审批模式门禁（`ModePlugin`）
     - `OBSERVABILITY (100)`：调用监控与 Trace 遥测包络（`TraceInterceptor`）
2. **洋葱包络执行规则**：
   - **`pre_execute`（从外向内）**：按优先级**从高到低**降序执行。一旦高优先级的 Guard 阻断，底层的 Mode 决不会无谓弹窗惊扰用户；
   - **`post_execute`（从内向外）**：按优先级**从低到高**升序反向解套（Unwinding），保证最外层的观测拦截器能完整包络整个生命周期耗时；解套后的改写结果（脱敏文本、错误状态标记等）双向同步回 `ToolExecutionResult`。
3. **安全递归互调（`ctx.invoke`）与死锁防御**：
   - 插件或工具可以通过 `ctx.invoke(sub_tool, arguments)` 安全互调其它工具；
   - 内核在 `ToolExecutionContext` 自动维护 `call_chain` 与 `depth`：
     - `depth >= max_depth`（默认 3 层）直接熔断，防御调用栈溢出；
     - 第一跳即记录根工具，实现自递归（`A -> A`）与跨工具环路（`A -> B -> A`）即时熔断，彻底杜绝死锁。
4. **流式进度上报（MCP `notifications/progress`）**：
   - `ToolExecutionContext` 接入 `ProgressReporter` 异步接口；
   - 工具在长耗时执行中调用 `ctx.report_progress(progress, total, message)`，经由通道直接流式推送给客户端，且子调用自动透明继承。

---

## 4. 上下文管理与 Prompt Cache

### 4.1 压缩触发

`should_compact(estimated) = estimated > max_context_tokens - reserve_tokens`（`pruning/strategy.rs:52-58`）。
默认 `max_context_tokens = 128_000`、`reserve_tokens = 16_384`、`keep_recent_tokens = 20_000`（`types/config.rs:45-56`）。宿主会用模型真实窗口覆盖 `max_context_tokens`（`root/src/host.rs:647`）。

**未越线时 `prune` 是 no-op**——这正是"两次压缩之间请求前缀字节级稳定"（prompt cache 全命中）的实现方式（`strategy.rs:60-78`）。

### 4.2 检查点压缩（唯一策略）

越线后（`strategy.rs:80-142`）：

1. `find_cut_index(context, keep_recent_tokens)` 从尾部回积累 token，取切点；**不落在 tool result 上**（必须与 tool call 同生共死），优先落在下一条 user 消息（轮次边界），不切入 index 0 的系统提示与 index 1 的既有检查点（`checkpoint.rs:159-210`）。
2. `region_floor`：有既有检查点时从 2 开始，否则从 1（`checkpoint.rs:137-147`）。
3. 序列化旧区域（tool result 截 2000 字符、tool args 截 200 字符，`checkpoint.rs:27-28`），机械提取读/写文件清单（按 `ToolEffect` 分类，`checkpoint.rs:306-342`）。
4. `Summarizer::summarize` 一次性 LLM 调用，**首次用 `INITIAL_SUMMARIZATION_PROMPT`，有前一份摘要时用 `UPDATE_SUMMARIZATION_PROMPT`（迭代 UPDATE-merge）**（`checkpoint.rs`）。调用前先取 `Semaphore` 许可：`ContextPruningConfig::max_concurrent_summaries`（默认 2）限制的是**共享该限流器的所有 agent**——多个 agent 往往同时越过压缩阈值。许可覆盖整个重试对，获取过程可取消。
5. 摘要请求：`cache_retention = Some("none")`（不污染缓存）、`session_id = None`（不做亲和路由）、`thinking_level = None`（`checkpoint.rs:429-444`）；`finish_reason == "length"` 或空输出视为失败。
6. 成功后一次性替换区间为 `checkpoint_message`（`name = "context_checkpoint"`，`checkpoint.rs:23`）；已有检查点时 `replace_at(1, ...)`，否则 `insert_at(region_start, ...)`。

### 4.3 紧急裁剪（安全网）

仅当摘要不可用/失败**且**仍超硬窗口时：`emergency_trim` 原子丢弃最老完整轮次（`strategy.rs:175-192`）。它不是可配置策略，正常路径永不触发。

### 4.4 溢出自愈

`error_detector.rs` 用四条正则从供应商报错里解析真实上限（OpenAI / Anthropic / Range / Generic），命中后 `pruner.update_max_tokens(detected)` 并重压缩重试，**最多 2 次**（`engine.rs:615,647-670`）。流中断还有三分支恢复（`engine.rs:790-854`）：工具参数中断→让模型重发；正文中断→拼接续写（保留末尾 60 字符做锚点）；首 token 前中断→干净重试。

### 4.5 缓存保温

`cache/warmer.rs` 是 pi-coding-agent `CacheWarmer` 的移植：

- 每轮**确实命中缓存时**才被重新调度（替换上一代，要求 `cached_tokens + cache_write_tokens > 0`），`generation` 递增防串台（`engine.rs:984-989`、`warmer.rs:170-225`）；
- 刷新时延 = `TTL × 90%`，且至少留 10s 余量（`warmer.rs:82-90`）；
- 经济性闸门：`expected = probability × miss_cost − warm_cost`，`≥ $0.05` 才刷（`warmer.rs:98-130`）；
- 空转概率常数 **0.15**，空转时长上限 **30 分钟**（`warmer.rs:39-46`）；
- 模型必须同时在 `models.json` 声明 `promptCache` 与 `cost`，否则保温关闭（`warmer.rs:17-22`）；
- `replay_safe = false`（如 Anthropic 的 budget 参与缓存键）时永不调度（`warmer.rs:62-69,204-206`）。

---

## 5. 控制原语（宿主可持有）

| 原语 | 类型 | 语义 | 检查点 |
|---|---|---|---|
| 取消 | `tokio_util::sync::CancellationToken` | 不可逆 | 每轮开头、流式、工具执行、弹窗 |
| 暂停 | `PauseGate`（`AtomicBool` + `Notify`） | 可恢复 | **工具边界**，绝不打断在途工具（`engine.rs:1206-1220`） |
| 转向 | `SteerQueues::steering` | 本轮工具调用后、下轮请求前进入 | 轮次边界 |
| 追问 | `SteerQueues::follow_up` | 仅当 run 无事可做时进入 | 收尾处 |
| 队列模式 | `QueueMode::All` / `OneAtATime`（默认） | 单轮投递几条 | `steer.rs:44-73` |
| 清队 | `PendingQueue::clear()` | 把文本还给宿主编辑器 | `steer.rs:122+` |

`SteerAccepted` 事件在消息真正进入 transcript 的同一时刻发出，宿主无需猜测（`types/event.rs:141-156`）。`cancel` 与 `pause` 的竞争关系有测试钉住："cancel wins over a pause"（`tests/pause_gate_test.rs:121`）。

---

## 6. 事件模型

```rust
pub struct ObservedEvent { agent_id: String, event: AgentEvent, timestamp_ms: u64 }
```
（`types/event.rs:52-60`）

`AgentEvent` 全部变体（`types/event.rs:62-187`）：`TurnStart`、`TokenDelta`、`ReasoningDelta`、`ToolCallChunk`、`ToolCallReady`、`ToolExecStart`、`ToolExecResult`、`ToolTrace`（boxed）、`TelemetryNotice`、`TurnEnd`、`LoopComplete`、`ContextCompacted`、`SteerAccepted`、`Error`、`Custom`、`GateResult`。

两条投递路径（`engine.rs:1430-1460`）：

| 事件类别 | 通道 | 策略 |
|---|---|---|
| 微增量（`TokenDelta`/`ReasoningDelta`/`ToolCallChunk`） | broadcast + mpsc | `try_send`，**绝不阻塞引擎** |
| 结构化事件（生命周期、trace、业务事件） | broadcast + mpsc | 在 `Emitter::emit` 里**内联 `await`** `timeout(5 秒, send)`——**不 spawn**；超过 5 秒的送达被计数并 `warn!`（`EventDropCounters`，见 §18.5），宿主可从 `AgentLoop::event_drop_counters()` 得知记录不完整 |

**字节上界**（0.6）：通道同时受 `AgentConfig::max_event_bytes`（默认 8 MiB）约束。`EventBudget` 是字节信号量，生产者在入队前 `reserve(event.approx_bytes())`，`EventStream::recv` 消费时归还；单个事件大于整个预算时直接丢弃并计 `structural`（它永远塞不进去，截断又会改写宿主自己的内容）。所以内存上界不是 `512 × 单事件`，而是一个明确的数。

`AgentEvent::Custom { kind, payload }` 是工具/插件向宿主发业务事件的通用信封，内核**不解释** `kind`（`types/event.rs:162-172`）。随仓的 kind 有四个：`file_change`（代码包）、`mode_changed`（模式插件，§11.4）、`metrics_update`（遥测插件，§11.8.1）、`agent_status`（状态插件，§11.8.2）——前两个由工具/插件自己发，后两个由**宿主**在 `take_events` 里按 `seq` 变化追加。

---

## 7. 用户数据根与配置分层

`core/paths.rs` 是**唯一**决定 thunder 用户状态位置的地方：

```
解析顺序：$THUNDER_CONFIG_DIR → <home>/.thunder → <temp>/.thunder
```
（`paths.rs:22-25`）

```
<root>/models.json           provider 目录
<root>/auth.json             凭据
<root>/models_metadata.json  元数据缓存（写）
<root>/config.json           用户配置
<root>/mcp.json              MCP servers
<root>/THUNDER.md            用户级长期记忆
<root>/skills/               用户技能
<root>/logs/                 TUI 日志（写）
<root>/scratchpad/           超大工具输出（写）
<root>/conversations/        会话存储（写）
<root>/permissions.json     已同意的「always allow」规则（写，daemon / TUI 落盘）
```
（`paths.rs:9-20`）

**刻意不在根下**（用 `real_home_dir()`）：`~/.agents`、`~/.pi`、存 API key 的 shell rc、跨工具 `mcp.json` 位置、shell 路径的 `~` 展开（`paths.rs:27-33`）。宿主想屏蔽跨工具提示面，用 `THUNDER_SKILLS_NO_GLOBAL` / `THUNDER_NO_GLOBAL_INSTRUCTIONS`，**绝不伪造 `HOME`**。

工作区级 `<ws>/.thunder/` 的生活面（与数据根同名不同物）：

| 路径 | 读取者 |
|---|---|
| `config.json` / `config.local.json` | `thunder_config.rs:100-101` |
| `mcp.json` | `mcp/src/config.rs:129`（候选表第一项） |
| `memory/*.md` | `root/plugins/memory.rs:156,210`（由 `agent.memory` 声明，路径必须落在 `.thunder/` 内） |
| `plans/<session>.md` | `root/plugins/mode.rs:157`（plan 模式唯一可写目标，`<session>` 消毒成 `[A-Za-z0-9._-]`） |
| `models.json` / `auth.json` | 见下：**写**路径把项目级排第一，**读**路径不是 |

配置优先级：内置默认 → `~/.thunder` → `<ws>/.thunder` → `<ws>/.thunder/*.local.*`（`.thunder/README.md:46-47`）。`.arp/` 属于 `agent-resume-panel`，Thunder 不读（`.thunder/README.md:8-10`）。

> **`models.json` 的读写已统一为同一条链（已修复）**：读走 `ConfigSource::default_chain` 合并（用户级先赢），写走同一条链——`utilityModel` 落到读序第一个已存在文件（用户优先），探针结果落到读序第一个定义该 provider 的文件。修之前的缺陷是写走 `ModelsFile::primary_config_path`（项目优先、CWD 相对），探测回写会被下一次加载覆盖；`ModelsFile::load_default` 也已收敛到同一合并语义，不再是第三种读法。回归测试：`providers/src/source.rs` 的 `write_goes_to_first_existing_in_read_order` / `owner_is_the_first_definer_in_read_order` / `write_target_is_visible_to_the_merged_read`。

---

## 8. 其余 crate（细节见各自小节）

- [`thunder-agent-pack-code`](#9-能力包thunder-agent-pack-code) — 6 个内建工具、`CodeScope`、`FILE_MUTATION_LOCKS`、工程提示词
- [`thunder-agent-providers`](#10-模型层thunder-agent-providers) — `models.json` 目录 + `RpiAiClient` 三种方言
- [`thunder-agent-root`](#11-微内核宿主thunder-agent-root) — 插件选择、四档模式、记忆、配置合并
- [`thunder-agent-daemon`](#12-sidecarthunder-agent-daemon) — NDJSON 协议 2、并发调度、UI 反向 RPC
- [`thunder-agent-runtime`](#13-无头运行时thunder-agent-runtime) — JSONL 协议 2、远程工具、宿主完成判定
- [`thunder-agent-skills`](#14-技能引擎thunder-agent-skills) — SKILL.md 解析、意图触发、TTL 缓存
- [`thunder-agent-mcp`](#15-mcp-客户端thunder-agent-mcp) — stdio JSON-RPC、动态工具桥接
- [`thunder-conversation`](#16-会话存储thunder-conversation) — Fs/内存双模、index.json、多拓扑阶段
- [`thunder-tui`](#17-终端界面thunder-tui) — Ratatui 全宽终端、流式渲染、审批弹窗

---

## 9. 能力包：`thunder-agent-pack-code`

### 9.1 定位

内核不知道"什么是 shell、工作区、文件"（`pack-code/src/lib.rs:3-6`）。装了这个包，`AgentLoop` 才得到：6 个内建工具、`CodeScope`、原子写 + 进程级 per-path 锁、`file_change` 变更上报、工程默认提示词（`lib.rs:7-22`）。

一键安装：`install_code_pack(agent, &CodePackConfig{ register_builtins, apply_default_prompt })`（`pack.rs:37-66`）——它同时装 scope 与工具，因为"问路径"和"不知道路径在哪的内核"是两个不同的问题（`pack.rs:29-31`）。所有工具都 `with_default_cwd(ws)`，相对路径 = 相对工作区。
**注意**：`install_code_pack` 在生产路径上**没有调用方**——`thunder-agent-root/src/host.rs:675-680,696-721` 与 TUI 的单 Agent 路径（`tui/src/app.rs:4312,4356-4371`）都是自己逐个 `register_tool` 并把 `CodeScope` 单独装上去；只有 `pack-code/tests/code_tools_test.rs:105` 用这个便捷函数。同样，`CodePackConfig::apply_default_prompt` 声明了但**从未被读取**（`pack.rs:14,21`）——默认提示词实际由宿主显式取用（`root/src/host.rs:563` 的 `DEFAULT_AUTONOMOUS_SYSTEM_PROMPT`）。

### 9.2 工具清单

| 工具 | 文件 | 参数 | 要点 |
|---|---|---|---|
| `read_file` | `builtin/fs.rs:29-111` | `path`, `offset`, `limit` | 单次读取上限 **10 MB**，但该检查只在 `limit.is_none()` 时生效（`:84`）——传任意 `limit` 即绕过；随后仍是整文件 `read_to_string` 再切行（`:93-109`），所以"分块"只限制输出、不限制内存 |
| `write_file` | `builtin/fs.rs:129-429` | `path`, `content` | 原子写 + per-path 锁，见 §9.4 |
| `bash` | `builtin/bash.rs` | `command`, `cwd?`, `timeout_ms?` | 默认超时 **120s**（`:93`），`bash -c`，stdin=null 防交互挂死（`:98`），环境变量脱敏（`:285-299`），Unix 下 `process_group(0)` + 超时 `kill(-pid, SIGKILL)` 杀整组（`:117-118,254-265`） |
| `grep` | `builtin/search.rs` | pattern/path/… | ripgrep 内核**进程内**嵌入（`grep-searcher`+`grep-regex`+`ignore`），非子进程；上限 100 行匹配、单行 500 字符、输出 50 KB（`:20-26`） |
| `find` | `builtin/fs_ext.rs` | pattern/path | `ignore` crate 遍历（.gitignore 感知），上限 1000 条 / 50 KB（`:15-17`） |
| `ls` | `builtin/fs_ext.rs` | path | 每目录上限 1000 条（`:19`） |

`bash` 的环境脱敏（`bash.rs:270-299`）：键名含 `KEY|TOKEN|SECRET|PASSWORD|AUTH|CREDENTIAL` 或以 `AWS_|GITHUB_|OPENAI_|ANTHROPIC_|DEEPSEEK_` 开头的一律 `env_remove`，除非在 `THUNDER_ENV_PASSTHROUGH`（逗号分隔）里显式放行。另外注入 `DEBIAN_FRONTEND=noninteractive`、`CI=true`、`TERM=dumb`、`PAGER=cat`、`GIT_TERMINAL_PROMPT=0`（`:107-111`）。
`CodeScope::new` 的文档写"with the pack's default protections in place"，但构造函数把 `protections`/`write_blocked` 都置空（`scope.rs:96-105`）；保护实际由 `from_agent_config` → `with_thunder_data_root` 施加（`:79-94,135-149`）。文档与实现不符。

### 9.3 `CodeScope`：知识一半

```rust
pub struct CodeScope {
    workspace_root: PathBuf,
    roots: Vec<PathBuf>,        // 视为工作区内的根（含 extra_workspace_roots 与 scratchpad）
    protections: Vec<PathBuf>,  // 双向禁区（凭据库）
    write_blocked: Vec<PathBuf>,// 可读不可写（thunder 数据根）
}
```
（`scope.rs:63-72`）

`from_agent_config`（`:79-94`）的根集合 = workspace_root ∪ `extra_workspace_roots` ∪ `scratchpad.base_dir`（溢出的工具输出要被模型读回，必须算工作区内，`:89-92`）。

`classify(target, effect)` 的顺序（`scope.rs:160-198`）：

```
candidate()：展开 ~ / $HOME / ${HOME}；相对路径按 workspace_root 解析   :202-209
resolve_for_check()：逐段 canonicalize（解符号链接）→ normalize_path    :250-269
  ↓
是 WRITABLE_DEVICES（/dev/null /dev/stdin /dev/stdout /dev/stderr /dev/tty）？
  └ 命中 ⇒ **只跳过下面整段 Critical 判定**，并不返回 InScope；未命中 ⇒ 继续往下判  :169-172
命中 protections（auth.json / models.json / credentials.json，双向）？      → Critical  :174-176
是写操作？
  ├ 命中 write_blocked（数据根）？                                        → Critical  :178-180
  ├ 任一路径段是 .git？                                                    → Critical  :181-183
  └ 命中 PROTECTED_WRITE_PREFIXES（/etc /dev /proc /sys /boot /bin /sbin
      /usr /var/db /System /Library /Applications …）？                    → Critical  :184-188
在某个 root 前缀下？ → InScope；否则 → OutOfScope（不是拒绝）              :191-197
```

> `WRITABLE_DEVICES` 那一格容易读错：它是 `if !device { …Critical 判定… }` 的一层**豁免**，不是一条 `InScope` 出口。设备节点最终仍要走最后的 roots 判定 —— `/dev/null` 不在任何 root 下，因此**写 `/dev/null` 是 `OutOfScope`**（`scope.rs:497-509` 的测试断言的就是这个）。上一版这里写成 `→ InScope`，与代码相反。

`scope_of` 的硬规则（`:213-227`）：**`Exec` 与 `Other` 一律 `Unknown`**——shell 命令的目标只有运行时才知道，`rm -rf $BUILD_DIR` 的诚实分类就是"本文件无法判断"；`Unknown` 不赚取自动放行，于是它变成一次询问而不是一次静默拒绝。

`resolve_for_check`（`:250-269`）是全仓唯一的符号链接逃逸检查，逐段 canonicalize 已存在前缀、不存在段按字面拼接；`normalize_path`（`:271-286`）做纯词法 `.`/`..` 归一。

### 9.4 原子写与 `FILE_MUTATION_LOCKS`

```rust
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> = ...;
```
（`builtin/fs.rs:383-384`）

写入流程 `write_cancellable`（`fs.rs:202-265`）：

1. `file_lock_for(&path)` 取该物理路径的 mutex（键为 `lock_key`：能 canonicalize 就用真实路径，否则用"canonicalize 后的父目录 + 文件名"，所以相对/绝对/符号链接父目录的两种拼写不会拿到两把锁，`:418-429`）；
2. `tokio::select!` 竞速：**取消令牌 vs 写入**——承诺是"被取消的 run 让目标要么原样、要么完整写入"（`:218-223`）；
3. `stage_and_commit`：`create_dir_all` → 记录原权限 → 在**目标同目录**建隐藏暂存文件 `.{name}_{pid}_{nanos}.tmp`（同文件系统 ⇒ rename 原子，无跨设备回退问题）→ `write_all` → **`sync_all`（先落盘再改名，否则崩溃后会留下正确命名的空文件）** → 恢复权限 → `rename`（`:266-347`）；
4. `TempFileGuard` 在任何退出路径（含取消时 future 被 drop）删除未提交的暂存文件（`:352-374`）；
5. 写完后 `release_file_lock` 在仍持锁时清理表项，且只在 `Arc::strong_count == 2` 时删除，保证排队中的写者仍持有同一把 mutex（`:404-416`）。

锁是**进程内**的：两个 daemon 或 daemon 与 TUI 之间不通过它协调（`:376-381` 注释）。

### 9.5 变更上报

`file_change.rs` 定义了 wire 契约常量 `FILE_CHANGE = "file_change"`（`:16-17`，**不要改名**）：

- `write_file` 每次写后 `emit_write(sink, path, bytes, is_err)`（`fs.rs:169-176`）；
- `bash` 在**命令前后**各取一次 `git status --porcelain` 快照并差分（`bash.rs:84-87`、`file_change.rs:34-88`）——shell 的文件副作用从参数看不出来，这是唯一仍需要快照的地方；且**无论命令成功与否都上报**（`git add` 后 `git commit` 失败同样移动了工作区，`bash.rs:239-243`）。

内核只是把 `AgentEvent::Custom { kind: "file_change" }` 原样转发（`file_change.rs:8-11`）。

### 9.6 工程默认提示词

`DEFAULT_AUTONOMOUS_SYSTEM_PROMPT`（`prompt.rs:43`）分两段。

**行为段**核心是"先理解再动手"：一句话能说清就不写计划；只读工具随便用；真实爆炸半径的改动先与用户敲定方案（plan 模式时 harness 强制：`write_plan` → `exit_plan_mode`，批准是唯一出口）；意图不明就问，最多三问、每题带具体选项；结尾给出 2-3 个下一步选项。**计划本身不是提示词要求的阶段，而是用户控制的机制**（`prompt.rs:7-10`）。

**展示段**（`# Presentation`）是"输出形态按读者要比较什么来选"：要比较的数字或选项用 Markdown 表格（三行以上就上表，不写成带从句的段落）；步骤/决策/因果/依赖用一层嵌套列表；代码、命令、路径、配置用带语言标记的围栏；改过的文件只给改动 hunk，不整文件回贴；长回答先结论后证据；不堆装饰（无 emoji、无分隔线、不逐条加粗、不写单行表格）。

#### 图表规则按宿主能力装配

展示段是**渲染器的契约**，所以它不是一块固定文本：`RenderCapabilities`（`prompt.rs:29`）声明"这个宿主能画什么"，`autonomous_system_prompt(capabilities)`（`prompt.rs:68`）是唯一入口。

| 宿主 | `diagrams` | 结果 |
|---|---|---|
| Thunder TUI | `false`（默认） | 只有表格 + 带语言围栏；围栏在 TUI 里每行截 3 行（`tui/src/ui/markdown.rs`），所以它从不被要求画图 |
| 桌面可视化聊天 | `true` | 追加一条 ` ```mermaid ` 规则（`prompt.rs:40`） |
| 桌面定时任务 / headless | 不发送 | 保持默认，没人看它的转录 |

`RootRunOptions::render_capabilities`（`host.rs:58`）承载该字段；daemon 侧 `run_task` 接受可选的 `renderCapabilities`（`protocol.rs`，camelCase 别名），**缺省即不画**——旧客户端不会突然收到一个它渲染不了的围栏。桌面只在可视化聊天那条 IPC 上把它设为 `true`（`thunderIpc.ts`），定时器不设（`thunderScheduler.ts`）。

拼接方式值得注意：图表规则插在 `Skip the decoration.` 之前（`DECORATION_MARKER`），因为约束段必须最后——把"别堆装饰"放在规则前面读起来像又一条建议。`prompt_test.rs` 断言了这个顺序，以及默认变体里不得出现 `mermaid`。

### 9.7 已知细节

- `install_code_pack` 在生产路径无调用方；`CodePackConfig::apply_default_prompt` 从未被读取（见 §9.1）。
- `CodeScope::new` 的文档声称带默认保护，实际置空（见 §9.2）。
- 仓库里存在一个 1 字节的被跟踪文件 `thunder-agent-pack-code/x`（`git ls-files` 可见），是测试写入溢出留下的化石，不是有意资产。
- `expand_home`（`scope.rs:288-302`）以 `std::env::var("HOME").ok()?` 开头：**`HOME` 未设置时所有 `~` 路径直接退化为 `Scope::Unknown`**（fail-safe 方向，但意味着一次多余询问）。

---

## 10. 模型层：`thunder-agent-providers`

### 10.1 目录与配置来源

`ConfigSource`（`source.rs:6-63`）只有两级：`thunder_user`（`<数据根>/models.json` + `auth.json`）与 `thunder_project`（`<ws>/.thunder/models.json` + `auth.json`）。`load_merged`（`source.rs:62-82`）按顺序合并，**先到先得**（`entry(id).or_insert(...)`），即用户级 provider 定义优先于项目级。

`ModelsFile` schema（`config.rs:3-117`，camelCase + 多别名）：

```jsonc
{
  "utilityModel": "provider/model-id",   // 别名：utility_model / toolModel / fastModel / namingModel
  "providers": {
    "<provider-id>": {
      "baseUrl": "…", "api": "openai-completions", "apiKey": "…", "headers": {},
      "thinkingLevels": ["off","low","medium","high"], "defaultThinkingLevel": "medium",
      "thinkingLevelMap": { "high": "high" },
      "compat": { "supportsDeveloperRole": true, "supportsReasoningEffort": true,
                  "maxTokensField": "max_tokens", "…": "prompt-cache 相关 flag" },
      "models": [{
        "id": "gpt-4o", "name": "GPT-4o", "reasoning": false,
        "input": ["text","image"],        // 别名 modalities；省略 = 纯文本
        "contextWindow": 128000, "maxTokens": 16384,
        "cost": { "input": 2.5, "output": 10, "cacheRead": 1.25, "cacheWrite": 3.75 },
        "promptCache": { "short": 300, "long": 3600 },
        "thinkingLevelsProbed": false
      }]
    }
  }
}
```

关键默认值：`DEFAULT_SAFE_CONTEXT_WINDOW = 128_000`（`catalog.rs:9`）。`ModelSpec::prompt_cache_warm_settings(thinking)`（`catalog.rs:194`）是保温策略的唯一来源——**必须同时声明 `promptCache` 与 `cost`**，否则 `decide_warming` 直接返回 `Stop("cache economics unavailable")`。

`ProviderRegistry::load_default` / `load_from_sources`（`catalog.rs:359-384`），`probe_unprobed_models`（`:392`）会对未探测模型做能力探测，结果通过 `ModelsFile::update_model_thinking_probed_config`（`config.rs:231-299`）**回写 `models.json`**（`primary_config_path`，`:210`）——不是回写 `models_metadata.json`：后者只存上下文窗口探测结果（`ModelMetadataCache { context_windows }`，`catalog.rs:74-83`）。回写的落点有一个已核实的坑，见 §7 末尾关于读写优先级不一致的说明。

### 10.2 `RpiAiClient`：进程内流式传输

```rust
pub struct RpiAiClient { provider: Arc<dyn rpi_ai::Provider>, model: rpi_ai::Model, timeout: Duration }
```
（`client.rs:49-53`）

**三种方言**，其余明确报错（`client.rs:85-108`）：

| `api` | provider 实现 |
|---|---|
| `openai-responses` | `OpenAiResponsesProvider` |
| `openai-completions` | `OpenAiCompletionsProvider` |
| `anthropic-messages` | `AnthropicProvider` |
| 其它（gemini/bedrock/vertex/mistral…） | `Err(unsupported_api_message)` → `"api `{api}` is not streamable by this build; supported: …"`（`convert.rs:168-173`） |

流式实现（`client.rs:130-199`）：`stream_chat` 起一个 tokio task 消费 `AssistantMessageEvent`：

- `TextDelta` → `LLMStreamChunk::Token`；
- `ThinkingDelta` → `LLMStreamChunk::ReasoningToken`；
- **tool-call delta 被刻意丢弃**：消费方约定工具调用在终止事件上整体到达，`LLMStreamChunk::ToolCallChunk` 因此保持未使用（`client.rs:174-176`）；
- `Done` → `Completed{…}`；`Error` → `Err`；
- **流在无终止事件时结束** → 显式报错 `"rpi-ai stream ended without a terminal event (N chars of text received)"`，而不是静默关通道（`client.rs:188-195`）。

通道深度 `CHUNK_CHANNEL = 64`（`client.rs:47`）。

wire 语义（`client.rs:1-19`）：`prompt_tokens` 是**完整** prompt（rpi-ai 的 `input` 只是未缓存部分，缓存桶被折回）；`finish_reason` 沿用旧 bridge 词汇：`toolUse`/`deferred` → `tool_calls`，`length` → `length`，其余 → `stop`。

已知缺口如实上报（`client.rs:14-19`）：`top_p` 在 rpi-ai 无一等字段，只能塞进 `Model::sampling_params`（OpenAI 形状 provider 会合并进请求体，Anthropic provider 忽略该 map）。

请求映射 `build_options`（`client.rs:205-232`）：`thinking_level` → `reasoning`；`cache_retention` 映射 `none`/`short`/`long`（缺省 = short，`:236-243`）；取消令牌**桥接**——因为 rpi-ai 只认自己的 signal，所以起一个 task `await` 调用方的 token 再取消自己的（`:211-218`）。

上下文映射 `build_context`（`client.rs:245+`）：多条 `System` 消息**拼接**成一条 system prompt；`User.parts` 非空时映射为多模态 blocks；`Assistant` 的文本 + tool_calls 映射为 content blocks；空 assistant 消息被丢弃（否则会序列化成空 content 数组）。

### 10.3 `thunder-agent-runtime`：JSONL 无头运行时

二进制 `thunder-runtime`，协议版本 `"2"`（`runtime/src/session.rs:17`）。

**请求**（`protocol.rs:40-106`）：`ping` / `start` / `tool_result` / `gate_result` / `cancel`；全部带可选 `id` 与 camelCase 别名（`runId`、`callId`、`maxTurns`、`thinkingLevel`、`requestTimeoutMs`、`sessionId`）。

**事件**（`protocol.rs:112-158`）：`response` / `observation`（原样转发 `AgentEvent`）/ `tool_call` / `gate_request` / `run_finished`（带完整 `messages` 供宿主持久化）/ `error`。

远程工具 `RemoteTool`（`remote.rs`）：声明给模型，但 `execute` 不做事——把调用转成 `RuntimeEvent::ToolCall` 发给宿主，然后在 oneshot 上等 `tool_result`。pending 表按 **`(run_id, call_id)`** 索引，所以发给 A run 的结果不可能答到 B run 的同名调用。宿主不答（sender 被 drop）→ `Err("the host dropped the tool call without answering")`；宿主已走 → 清理 pending 并 `Err("the host is gone; …")`；run 被取消 → `select!` 立刻返回 `Err("run cancelled while the host was executing the tool")` 并清掉 pending，不把任务挂到宿主答话为止。

`RemoteGate` 实现 `CompletionGate`：完成判定同样发到宿主（`gate_request`）等 `gate_result`，同样监听 `GateRequest::cancellation_token`——run 在等待裁决时被取消，直接按 `Pass` 放行（唯一能让循环结束的裁决），并清掉该 run 的槽位。`parse_effect`（`remote.rs:19-29`）把宿主声明的 `read|write|exec|plan_artifact` 映射到内核 `ToolEffect`，**未声明一律 `Other`（按变更处理）**。

运行器形态：`Runtime`（`session.rs:19-40`）持有 `runs: HashMap<run_id, CancellationToken>`、两个 pending 表，以及一个 `mpsc::UnboundedSender<RuntimeEvent>` → 单一 writer task，保证一帧一行、不交错。

---

## 11. 微内核宿主：`thunder-agent-root`

### 11.1 装配契约

0.6 起宿主里不再有"插件"这个概念。一个宿主对一次运行的全部贡献是四个显式列表加一个按运行准备钩子（`assembly.rs`）：

```rust
pub struct RunEnv {                       // 一次运行的全部上下文
    session_id: String, workspace: Option<PathBuf>, route: Option<String>,
    prompt: Option<String>, ui: Arc<dyn HostUi>, cancellation: CancellationToken,
}

pub trait PromptSection: Send + Sync {    // 系统提示词的一段
    fn id(&self) -> &str;
    fn order(&self) -> i32 { 100 }
    fn render(&self) -> Option<String>;
}

pub struct ToolPack {                     // 工具与 tool plugin 的集合
    id: String,
    tools: Vec<Arc<dyn AgentTool>>,
    tool_plugins: Vec<Arc<dyn ToolPlugin>>,
}

pub trait Observer: Send + Sync {         // 事件的观察者
    fn id(&self) -> &str;
    fn on_event<'a>(&'a self, event: &'a AgentEvent, env: &'a RunEnv) -> BoxFuture<'a, ()>;
    fn on_finish<'a>(&'a self, result: &'a AgentRunResult, env: &'a RunEnv) -> BoxFuture<'a, ()>;
}

pub trait Prepared: Send + Sync {         // 按运行做准备
    fn id(&self) -> &str;
    fn prepare<'a>(&'a self, env: &'a RunEnv) -> BoxFuture<'a, Result<(), String>>;
}
```

`Assembly` 只做四件事：`prompt(base)`（按 `order` 拼接）、`tools()` / `tool_plugins()`、`dispatch_event` / `dispatch_finish`、`without_disabled(&HashSet)`（`plugins.disabled` 的唯一减法）。`ids()` 是 daemon 的 `active_plugins` 字段与日志的来源。

**为什么不是 trait 对象大杂烩**：旧 `ThunderPlugin` 把提示词、工具、gate、生命周期塞进同一个形状，于是每个插件都拿到一个足够宽的 `PluginContext`；而四件事的失败方式、调用时机、同步/异步要求都不同（渲染必须同步，观察必须异步，工具只在内核漏斗里被调用）。

`PluginContext` 仍在（`context.rs`），但它现在只服务**工具执行期**：session / route / ui / mode / tool-slot。运行装配不再经过它。

### 11.1.1 一次 run 的三个阶段

| 阶段 | 谁做 | 做什么 |
|---|---|---|
| **装配** | `ThunderRoot::execute` | 解析 route/session、绑定 mode、收集 sections/packs/observers/prepared，`without_disabled` |
| **准备** | `Assembly::prepare(&RunEnv)` | 逐个 `Prepared`：memory 读工作区并缓存渲染块、skills 扫描目录、mcp 发现工具、web 解析后端配置、mode 绑定会话与 plan 路径 |
| **运行** | `AgentLoop` + 事件代理 | `take_events` 每收到一条事件先 `assembly.dispatch_event(&event, &env)` 再透传；`join` 时 `dispatch_finish` |

`Prepared` 的顺序就是注册顺序；`ThunderRoot` 在 `execute` 里先跑 mode 的 `prepare`（绑定 plan 路径与会话），再跑 `Assembly::prepare`。

**没有"两阶段握手"了**：旧设计里 `on_init` 之后还有 `on_run_ready`，只为把 tool invoker 交给插件；`RunEnv` 现在就带着它需要的一切，不需要第二段。

### 11.2 注册即激活

旧的 `PluginSelector`（关键词 + 会话锁定 + `SelectionCache`）已删除（PR #17 的最后一步，PR #18 收尾）。

- 一个宿主注册了哪些 section / pack / observer，这次运行就有哪些。没有"按提示词挑子集"。
- `plugins.disabled`（`.thunder/config.json`）是唯一的减法，由 `Assembly::without_disabled` 一次性应用。
- `forced_plugins` / `PluginSelection` / `with_plugin` 全部删除；`RootRunOptions` 不再有"这次运行用哪些插件"这个字段。

理由：关键词选择会**无声地**改变工具集与提示词前缀——模型不知道、用户不知道、provider 的 prompt cache 也不知道；而它真正被使用的场景只有一个（`mcp`）。

### 11.3 `ThunderRoot::execute` 装配序列

1. 取 prompt（`Text` 原样；`Messages`/`Buffer` 取最后一条 user，否则 `"General task"`）
2. `ThunderConfig::load(workspace)`
3. session id（缺省 `sess_<ms>_<seq>`）+ route id（`run_{session}_{seq}`）
4. 解析 mode：宿主传入的 `ModePlugin` 复用（会话级规则/模式切换得以保留），否则新建；`set_mode` + `bind_session`
5. 以 `self.assembly.clone()` 为底，追加：
   - mode 的 `PromptSection` 与 `ToolPack`（`write_plan` / `exit_plan_mode`）
   - 默认 metrics（`Prepared` + `Observer`），除非宿主已用 `with_metrics_plugin` 注入
   - 默认 status（同上）
6. `assembly.without_disabled(plugins.disabled)`
7. `mode_plugin.prepare(&env)` 然后 `assembly.prepare(&env)`
8. 构造 `RunEnv`（session / workspace / route / prompt / ui / cancellation）
9. `PluginContext`（工具执行期上下文）+ 工具槽位
10. `ToolRegistry` / `ToolExecutor`：注册 `assembly.tools()`、`assembly.tool_plugins()`、`loader` 的声明式插件与拦截器；mode 作为 `ToolGate` **与** `InterceptorPlugin` 显式安装
11. `agent.with_tools_factory(StaticToolsFactory::new(registry, executor))`
12. `agent.start(...)`，返回 `RootRunHandle { handle, assembly, ctx, env }`

`RootRunHandle` 不再携带 `active_set` 或 `selection`；宿主读 `assembly.ids()`。

### 11.4 四档模式矩阵（`plugins/mode.rs`）

| mode | Read | Write·InScope | Write·OutOfScope/Unknown | Exec | PlanArtifact |
|---|---|---|---|---|---|
| `plan` | Allow | **Deny** | **Deny** | Allow（只读命令）/ **Ask** | **Deny** |
| `ask` | Allow | Ask | Ask | Ask | Ask |
| `acceptEdits` | Allow | **Allow** | Ask | Ask | Ask |
| `yolo` | Allow | Allow | Allow | Allow | Allow |

`PlanArtifact` 这一列**不属于任何一个具名分支**：`decide` 只对 `Read` / `Write` / `Exec` 分支，其余效果落进各模式的兜底臂——`plan` → `Deny`（`mode.rs:486-496`）、`ask` → `ask_for`（`:471`）、`acceptEdits` → `ask_for`（`:479`）、`yolo` → `Allow`（`:468`）。所以**四档下 `write_plan` / `exit_plan_mode` 都能过，靠的是按名字的豁免**（`:445-447`），不是靠 `ToolEffect::PlanArtifact`。上一版把这一列全写成 `Allow`，与本节的第 3 条自相矛盾。

三条承重实现细节：

1. **plan 跳过 allow 规则**：规则查询先于矩阵，但 `if mode != Mode::Plan` 把它挡在 plan 之外（`mode.rs:449-465`）——"更宽松模式里记住的 always allow 不是 plan 模式下写文件的办法"（`mode.rs:20-22`）。
2. **plan 拒绝文案必须给出出口**，否则模型会反复重试（见 `docs/permission-structure.md` §7）。
3. **plan 工具豁免按名字，不按 `ToolEffect`**（`mode.rs:438-447`）——否则任何工具都能自称 `PlanArtifact` 绕过（测试 `a_self_declared_plan_effect_is_not_an_exemption`）。
4. **plan 下的 Exec 先读命令本身**：内核的 `read_only::call_is_read_only`（`thunder-agent-loop/src/tools/read_only.rs`）能证明这条命令什么都不改时直接 Allow，否则照旧 `ask_for`。它只被 plan 分支使用——ask / acceptEdits 问的是"要不要跑点东西"，那里没有可省的问。判据、白名单与诚实的边界见 `docs/permission-structure.md` §7.5；测试在 `mode_matrix_test.rs::a_read_only_command_runs_in_plan_mode` 与 `read_only.rs` 自己的用例里，两侧都钉。

**每次模式变化都会被播报，且只在真的变了时播报。** 这条由 `ModePlugin::set_mode_announcing`（`mode.rs:392-399`）承担：它翻转并返回被替换掉的旧档位，档位本来就相同则返回 `None`。daemon 侧三条路都汇进 `emit_mode_changed`，推同一帧 `mode_changed`（`protocol.rs`）：

| 谁改的 | `source` | `task_id` |
|---|---|---|
| 客户端的 `set_mode` | `user` | 无 |
| `run_task` 钉了本次 run 的档位 | `run_option` | 有 |
| `exit_plan_mode` 被批准 | `plan_approved` | 有 |

为什么要"同值不播报"：宿主在**每次 run 开始**都无条件 `set_mode`（`host.rs:475`），若同值也播报，每轮都会冒一帧、面板很快就会学会忽略它。`get_permission_state` 对一个尚不存在的会话是**以所请求的档位创建**它（`service.rs:751`），因此那次 `set_mode` 也没有"转变"可播报——客户端从 `response` 的 `data.mode` 拿确认。

第三行是唯一 daemon 看不见的变化：`ExitPlanModeTool` 持有的是共享的 `Arc<ModeState>`（`mode.rs:575-577`）而非插件，它直接翻状态，只能通过**本次 run 的 event sink** 以 `AgentEvent::Custom{kind:"mode_changed"}`（`MODE_CHANGED_EVENT_KIND`）申报；daemon 的事件泵把它翻成上面那张表里的同一个顶层帧（`service.rs` 的事件泵）。custom 事件本身照旧原样转发，不吞。sink 是可选的，批准**没有 sink 也必须成功**。

范围限定：这条承诺覆盖的是 **daemon 的客户端**。TUI 是另一个进程里另一个 `ModePlugin` 实例，收不到这个帧。新增会改会话档位的调用点时，必须走 `set_mode_announcing` 才会被播报。

`Mode::parse`（`mode.rs:91-104`）接受历史名：`ask|default|manual|mutations|writes|always|all` → `Ask`；`acceptedits|accept_edits|shell_only|ask_shell` → `AcceptEdits`；`yolo|never|none|bypass|bypass_permissions|bypasspermissions` → `Yolo`。**`plan` 的语义被修正**（0.3 把它解析成 `never`，与"计划"完全相反）。`Mode::default() == Ask`（`mode.rs`）——secure by default；`yolo` 是显式 opt-in。

### 11.5 `AllowRule` 的窄化

```rust
pub struct AllowRule { pub tool: String, pub arg_prefix: Option<String> }
```
（`mode.rs:170-177`）

`for_call`（`mode.rs:195-226`）**绝不授予"一个工具类"**：

| 工具类型 | 规则形式 | `matches` 逻辑 |
|---|---|---|
| `Exec` | 命令到第一个链接符为止的前缀（空白已折叠） | `strip_prefix` 后余下部分**不得含** `; & \| \` > < \n $` |
| 其它（有 `path` 参数） | `path` **归一化后**（绝对化 + 折叠 `.`/`..`/重复与末尾分隔符） | `under(path, prefix)`：自身或其后代，按分隔符判 |
| 其它且无 `path` | `None` → **不提供** "Always allow" 选项 | — |

`SHELL_CHAINING = [';','&','|','`','>','<','\n','$']`（`mode.rs:211`）。于是 `git status` 永远不会授权 `git status && rm -rf /`。

**键在两侧都归一化**（`plugins/permission_rules.rs`：`:108` 命令、`:120` 路径、`:140` `under`）。规则存的是归一化后的键，判定时也把待判调用归一化——否则一条规则只对"弹窗当时那一种写法"生效，用户会以为 Always allow 坏了。工作区绑定时相对路径绝对化，`a.txt` / `./a.txt` / `/ws/a.txt` 才算同一个文件；`under` 取代 `starts_with(prefix + "/")` 修掉了尾斜杠（`src//`）与根目录（`/`）两个永不匹配/不覆盖的 bug。

**daemon 与 TUI 会把规则落盘**（`ModePlugin::with_persisted_rules`，`mode.rs:472`）：`<数据根>/permissions.json`（`permission_rules.rs:50`，即 `--config-dir`/`$THUNDER_CONFIG_DIR`/`~/.thunder`），`{"version":1,"rules":[…]}`，临时文件 + rename 写入、unix 上 `0600`；读失败一律当空（这个文件只会授予）。`ModePlugin::new`（测试、嵌入方、headless）仍是纯内存，行为与从前一致。用户级而非按工作区：跨项目再问一遍正是让人关掉询问的原因；规则仍然很窄，plan 下仍然不查，守卫仍然先跑。撤销＝改/删该文件（下次建会话生效）或宿主调 `clear_rules`。

### 11.6 plan 模式的两个工具

- `write_plan`（效果 `PlanArtifact`，`:542-544`）：参数 `{ content }`，整体覆盖写到 `<workspace>/.thunder/plans/<sanitize(session)>.md`（`plan_path`，`:157-162`）。`sanitize_session_id`（`:139-154`）把非 `[A-Za-z0-9._-]` 一律换成 `_`，`""`/`"."`/`".."` 落回 `"session"`——**jail 删除后这是真实注入点**（daemon 的 session_id 来自客户端），测试直接钉住 `../../../etc/evil` 逃不出去。写入复用 `WriteFileTool::write_cancellable`，同一把 per-path 锁与原子语义。
- `exit_plan_mode`（效果 `Read`，`:592-594`）：**忽略模型传来的任何参数**，harness 自己从磁盘读计划。文件缺失/空白 → `is_error` 且**不弹窗**。否则弹 `UiRequest::Review`，四个选项（`:125-128`）：`Yes, and auto-accept edits` / `Yes, but ask before each edit` / `Yes, fully autonomous` / `No, keep planning`。**只有用户的答案能改变模式**，无法识别的答案什么都不翻（`:643-684`）。批准（`approve`，`:732-756`）在翻完档位后往本次 run 的 event sink 发一条 `mode_changed`（带 `mode`/`previous`/`source:"plan_approved"`，常量 `mode.rs:130-135`），daemon 据此推 `mode_changed` 帧——这是 daemon 自己看不见的那条变化（见 §11.4）。**tool result 本身就是审批的审计行**：批准返回 `Plan approved by user (choice: "<选项原文>" -> <mode>, prev: <previous>, source: plan_approved). Mode is now <mode>. …`（选项原文逐字回显，`prev` 是被替换的档位）；`No, keep planning`、无答案、无法识别的答案分别带 `Decision: keep_planning …` / `Decision: dismissed …` / `Decision: unrecognised …` 前缀（含 `source: plan_approved`），反馈正文保持逐字嵌入。字符串刻意保持一两行——tool result 超大会被 scratchpad 换成 handle。"No, keep planning" 与无答案都**不播报**：没有变化就没有事件（但 tool result 里的 `Decision:` 前缀照带）。

### 11.7 记忆与配置

记忆加载顺序（`.thunder/README.md:89-97`、`plugins/memory.rs`）：

1. `<数据根>/THUNDER.md`
2. `<ws>/.thunder/THUNDER.md`
3. `<ws>/.thunder/memory/*.md`（按文件名排序）
4. `<ws>/.thunder/THUNDER.local.md`
5. `config.json` 的 `agent.memory.files[]`

`@path/to/file.md` 导入：相对路径按**导入文件所在目录**解析，越狱防护（必须落在工作区或数据根内）、深度上限 **5**、总量上限 **256 KB**、重复/循环去重（`.thunder/README.md:99-103`）。

**提示缓存稳定性**（`.thunder/README.md:105-109`）：记忆块位于每次请求的 position 0（正是 provider 缓存键控区域），所以**每次 host 初始化只读一次**并缓存；编辑在下次会话生效。`MemoryPlugin::render` 在 `memory.enabled == false` 或没有绑定工作区时返回 `None`。

配置分层（`thunder_config.rs:96-104`）：`$THUNDER_CONFIG_DIR/config.json` → `<ws>/.thunder/config.json` → `<ws>/.thunder/config.local.json`；合并规则是**只在 overlay 声明该字段时才替换**（`:131-148`）；`apply_to` 只改 `max_turns` 与 `thinking_level`。

**插件声明式配置（`plugins` 块）**：在 `.thunder/config.json` 中支持通过 `plugins` 字段配置插件启用、禁用与外部 MCP 服务注入：
```json
{
  "plugins": {
    "builtin": ["read_file", "write_file", "bash", "grep", "find_by_name", "list_dir"],
    "disabled": ["bash"],
    "mcpServers": {
      "github": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": { "GITHUB_TOKEN": "..." }
      }
    }
  }
}
```
- `builtin`：内建工具显式列表（默认全启）。
- `disabled`：禁用列表，支持双向别名匹配（如禁用 `bash` 或 `read`/`read_file` 均会自动匹配其规范名称及别名）。
- `mcpServers`：声明式外部 MCP 进程定义，由 `PluginLoader` 在初始化时拉起并以双模 `McpToolBridge`（同时实现 `AgentTool` 与 `ToolPlugin`）注入工具漏斗。

上下文文件发现（`instructions.rs`）：候选 `AGENTS.override.md` → `AGENTS.md` → `AGENTS.MD` → `CLAUDE.md` → `CLAUDE.MD`（每目录首个命中胜出）；全局目录 `<real_home>/.agents`、`<real_home>/.pi/agent`、`<数据根>`，**只加载第一个命中**；工作区祖先自底向上再反转（最外层在前）；剥离 UTF-8 BOM。开关：`THUNDER_NO_GLOBAL_INSTRUCTIONS` / `THUNDER_SKILLS_NO_GLOBAL`（真值 `"1"`/`"true"`，大小写不敏感）。

### 11.8 trace 收集（宿主侧）

`TraceCollector`（`trace_collector.rs`）：`record(ObservedEvent)` + `record_tool_trace(ToolTrace)`，上限 `MAX_EVENTS = 5000`、`MAX_TOOL_TRACES = 2000`、`MAX_DECISION_RECORDS = 2000`；微增量只计数不保留；**丢弃是自报的**——`dropped_deltas` / `dropped_events` 被计数，`is_complete()` 在任何丢弃后为 false，且淘汰日志本身也有上限（`:111-113,273-280`）。落盘位置由宿主决定：`<store>/<session>/traces/<task_id>.json`，**route 就是 task_id**。

### 11.8.1 遥测插件：`MetricsPlugin`

`MetricsPlugin`（`plugins/metrics.rs`）是默认装配的一部分（宿主没有 `.with_metrics_plugin(...)` 时 `ThunderRoot::execute` 追加一个默认实例），把**与模型有关的全部遥测**——实时/平均 tok/s、token 明细、prompt cache 命中率、上下文占用、纯解码耗时与工具耗时——从内核搬到插件层：

- **零侵入内核**：它只实现 `Observer`（`on_event`/`on_finish`）与 `Prepared`（`begin_run`），不注册工具、不装 gate，事件派发走 `EventBudget` 旁路，内核主循环不知道自己被观测。
- **生成时钟只计解码**：收到本轮第一个 `TokenDelta`/`ReasoningDelta` 时开表，`ToolExecStart` 或 `TurnEnd` 时结算并暂停——TTFT（prefill + 排队）与工具执行都不计入分母。
- **一份权威快照**：`MetricsSnapshot`（`metrics.rs`，serde）由 `RootRunHandle::take_events` 随流以 `AgentEvent::Custom { kind: "metrics_update", payload }` 推送；`RootRunHandle::metrics_snapshot()` 与 daemon 的 `get_metrics` 命令可随时读取，trace 文件顶层的 `metrics` 键是同一结构。
- **两个数的语义不同，别混**：`context_tokens` 是**仪表**（当前这次请求的 prompt + 已生成量，每个 `turn_end` 被精确值**覆盖**）；`session_total_tokens` 是**计数器**（累计计费，每个 `turn_end` **累加**「该轮 prompt + completion」——每轮请求都会把整个 prompt 重新计费一次）。"一轮一次"不是不累加，而是增量只有轮末才由 provider 报出。
- **但两者在轮内也会前进**：`TurnStart` 现在带上 `estimated_prompt_tokens`（内核本来就在日志里算这个数），插件以它作为本次请求的 prompt 基数；此后每个 `TokenDelta` 都让两个数随生成量增长，`ToolExecResult` 把工具输出计入下一个请求的 prompt（`turn_end` 用精确值**赋值**覆盖基数，所以不会重复计）。这样面板上的「上下文 / 总消耗」在流式过程中就在动，而不是只在轮末跳一下。
- **消费方**：TUI 在 `handle_agent_event` 里反序列化 `metrics_update` 直接刷新状态栏；daemon 随 `observed_event` 一并转发给宿主，并把它写进 trace；桌面面板与 prototype-edit-llm 都以快照为准（含「总耗时」＝`wall_duration_ms`，它每帧都在刷，而 `turn_end` 的 `duration_ms` 只是**该轮**耗时）。
- `TraceCollector` 不保留 `metrics_update`：它是从流派生的高频遥测，最终快照已存在 trace 顶层，逐条保留既无信息量又会顶掉真实事件（`trace_collector.rs:59-68`）。

### 11.8.2 执行状态插件：`StatusPlugin`

`StatusPlugin`（`plugins/status.rs`）回答的是**另一个问题**：Agent 现在在干什么。遥测说「多少」，它说「什么」，两者不重叠。装配关系同遥测：宿主没有 `.with_status_plugin(...)` 时 `execute` 追加一个默认实例，所以经 `StandardHostBuilder` 装配的宿主必然发布状态。

- **词汇**：`AgentPhase`（serde `snake_case`）= `idle | thinking | streaming | tool | awaiting_user | paused | compacting | done | error`；载荷 `AgentStatusSnapshot { session_id, phase, tool?, turn, detail?, started_at_ms, seq }`（`tool`/`detail` 省略空值）。`done`/`error` 是黏滞终态（`is_terminal()`）；**`Cancelled` 刻意落在 `idle`**——它是"没有 run"的静止档，因此不算终态。
- **状态机折自事件流**：`TurnStart→thinking`、`TokenDelta|ReasoningDelta→streaming`、`ToolExecStart→tool`（记工具名）、**整批** `ToolExecResult` 到齐才回 `thinking`（并行调用不会在第一个结果落地时就掉回）、`ContextCompacted→compacting`（detail 记 token 变化）、`LoopComplete→done/error/idle`。**终态必须在 `on_event` 的 `LoopComplete` 里落定，不能放 `on_finish`**：`dispatch_finish` 跑在 `join()` 里，那时事件流已排空，再改状态没有接收方（§11.2 的 `dispatch_drained` oneshot 就是为这个顺序存在的）。run 落定后引擎的解流事件（`tool_exec_result` 之类）不会把状态"复活"。
- **两个只有宿主知道的阶段**：`awaiting_user` 与 `paused` 在事件流里**根本不存在**——审批/提问时 gate 阻塞在 `ToolExecutor::execute_all` 内部，内核一条事件都不发；暂停门同理。所以插件公开 `note_awaiting(session_id, bool)` / `note_paused(session_id, bool)`，由**弹窗的那一方**调用。等待是 `base` 之上的**视图**而非替换（取最新进入的那个等待，`set_base` 不碰等待）——工具轮里弹了审批，答完回到 `tool` 而不是 `thinking`；两个等待互不覆盖（可以既暂停又有个问题在屏幕上）。**清除必须 RAII**（daemon 的 `AwaitingGuard`，`ui.rs`）：一次等待有四条出口（作答 / 取消 / 超时 / 通道关闭），只在部分出口清的状态比不清更糟。
- **发布点只有一处**：`RootRunHandle::take_events`（`host.rs`）逐条转发后比对 `seq`，**变了才**追加 `AgentEvent::Custom { kind: "agent_status" }`——照 `metrics_update` 的先例，但按变化去重（遥测每帧都发，状态只在跃迁时发；`seq` 让消费者能丢弃乱序/重复帧）。该代理同时 `select!` 插件的 `watch` 变化信号：**宿主驱动的等待必须在没有任何内核事件在飞的时候也能被看见**，否则审批期间消费者看到的是一个"看起来在忙"的运行（这正是 `agent_status_stream_test.rs` 断言的那条）。
- **宿主侧补位的两条**：daemon 在 `ui.rs` 的 `dispatch` 用 `AwaitingGuard` 报 `awaiting_user`，在 `PauseTask`/`ResumeTask` 里按 `task_id → session_id` 报 `paused`（任务表与 session 是两套 id，`LiveQueue` 因此记着 `session_id`）。
- **消费方**：daemon 把它当普通 `observed_event` 原样转发给宿主（**协议不用升版**：`custom` 对协议透明）；TUI 不解析事件，直接读同一实例的快照并翻译成 OSC 7501 交给终端（§17.4）；宿主若自己驱动 `AgentLoop`（TUI 的 `/single` 直连路径不建 `ThunderRoot`），用 `StatusPlugin::begin_run` + `record_session_event` 手工喂养——这两个 API 存在就是因为那条路径没有插件宿主，而**两份状态机是不允许的**。
- **trace 里留着**（与 `metrics_update` 相反）：`TraceCollector` 只过滤遥测快照，状态帧会照常进 `events`。它低频（一次 run 通常个位数）且正是「宿主当时看到的是这个」的证据；TUI 的 trace 视图会以通用 `✦ agent_status` 行显示（和 `mode_changed` 一样走 `app.rs` 的兜底分支，没有专门渲染）。

### 11.9 已知细节

- 顺序稳定靠**注册顺序**：`Assembly` 的四个 `Vec` 按宿主 `with_*` 的调用顺序排列，`PromptSection` 再按 `order()` 排序；工具注册顺序因此跨实例确定，这是 prompt cache 前缀命中的前提。mode 的 section 总是最后追加（`execute` 里显式加入）。
- **一处仍无人拉的接线**：`ToolInvoker::invoke` 只有测试调用，没有随仓组件使用 `ctx.tool_slot()`（`ask_user` 工具在宿主侧直接用 `HostUi`，不走反向 RPC）。
- `ModePlugin::system_prompt_contribution` 在调用时重读 `self.mode()`，但热切换只对**下一次 run 的提示装配**生效；gate 本身立刻生效（`mode.rs:717-726`）。
- `write_plan` 走原子写，`memory_write` 用普通 `tokio::fs::write`（`mode.rs:561-563` vs `memory.rs:447,451`）。
- `RootRunHandle::join` 里 `dispatch_finish` 不返回错误（`Observer::on_finish` 的签名是 `()`）⇒ 会话持久化失败只会记日志，对调用方不可见。
- `RootRunOptions` **没有** `#[non_exhaustive]`：新增字段是下游破坏性变更。
- 缺省 session id 曾是常量 `sess_1`，导致同进程内未显式命名的 run 共享一份会话存储、一套模式规则与一个状态槽；现在退化为 `sess_<ms>_<seq>`（`SESSION_COUNTER` 原子自增，`host.rs`），宿主仍被要求自带 id 以便跨进程稳定。

---

## 12. Sidecar：`thunder-agent-daemon`

### 12.1 形态与约束

二进制 `thunder-daemon`，**NDJSON over stdio**：stdout 只走协议，所有日志强制 stderr（`main.rs:19-28`）。

```rust
pub const PROTOCOL_VERSION: u32 = 3;
```
（`protocol.rs:19`）

版本与 `CARGO_PKG_VERSION` **是两件事**：后者每次发版都动，前者只在宿主解析的请求/响应形状变化时动——`ping` 同时回 `{pong, version, protocol}`，宿主因此能区分"更新的 thunder"与"回答得不一样的 thunder"（`protocol.rs:6-19`）。

**各版本新增了什么**：`1`→`2` 是 0.4 那轮（见 `migration-0.4.md`）；`2`→`3` 是本轮——顶层多了**一条推送帧** `mode_changed`（§11.4）。它是**加法**的：不认得的宿主丢掉即可，所以这次升版是"记账"而不是握手，没有哪条路径拿它做门禁（不读 `ping` 的客户端照常工作，旧 daemon 只是永远不推这帧）。`daemon_ipc_test` 直接断言这个数字，改版本号必须同时改它。

启动参数（`main.rs:32-53`）：`--workspace`/`-w <path>`；`--config-dir <path>`（通过 `set_var(THUNDER_CONFIG_DIR)` 生效，让下面每一层只解析出一个根；缺值 `exit(2)`）；其余 argv 忽略。stdin EOF → `shutdown()` 取消所有存活令牌——**僵尸进程保证**（`main.rs:96-97`、`service.rs:357-362`）。

### 12.2 请求清单（`DaemonRequest`，tag = `method`）

| method | 关键字段 | 效果 |
|---|---|---|
| `ping` | `id` | `{pong:true, version, protocol:3}` |
| `list_models` | `id` | 重载 `ProviderRegistry`，返回模型目录 + `utility_model` |
| `list_conversations` | `id` | `store.list(filter.with_min_turns(1))` |
| `get_conversation` | `session_id` | 会话 JSON |
| `run_task` | `task_id`, `prompt`, `session_id?`, `model?`, `model_descriptor?`, `use_mock?`, `workspace_dir?`, `extra_workspace_dirs?`, `thinking_level?`, `attachments?`, `headless?`, `mode?` | 见 §12.3 |
| `pause_task` / `resume_task` | `task_id` | 翻 `PauseGate`；暂停时额外发 `task_paused` |
| `steer_task` | `task_id`, `message`, `behavior?`, `attachments?` | `behavior` 必须是 `steer`/`follow_up`，**缺失即报错**（不猜） |
| `clear_queue` | `task_id` | 清队并把文本回给客户端 |
| `answer_question` | `question_id`, `answers?`, `cancelled?` | 释放阻塞中的 `ask_user_question` |
| `answer_ui` | `request_id`, `value?`, `confirmed?`, `cancelled?` | 释放 `ui_request` 弹窗 |
| `get_permission_state` | `session_id` | `{mode, rules}` |
| `set_mode` | `session_id`, `mode` | 未知模式 → `success:false` + `unknown mode '…': expected one of plan \| ask \| acceptEdits \| yolo`，**绝不静默回落**；档位真的变了则**在 `response` 之后再推一条 `mode_changed`**（顺序如此，见 §11.4） |
| `cancel_task` | `task_id` | 移出 `active_tasks` 后 cancel，并记入 `stopping` |
| `reload_integrations` | `id` | 关 MCP 池 + 失效 selection cache（**新名字**：旧 `ReloadPlugins` 的 TS 插件宿主已删） |
| `get_trace` / `list_traces` | `session_id`, `task_id?` | 读 `<store>/<session>/traces/*.json` |
| `generate_title` / `set_conversation_title` | `session_id`, `force?` / `title` | 标题（manual 不被 auto 覆盖；标题截断到 40 **字符**） |
| `get_metrics` | `session_id` | 读共享 `MetricsPlugin` 的最新 `MetricsSnapshot`（未命中 → `success:false`） |

### 12.3 `run_task` 编排

准入优先（`service.rs:1061-1073`）：`concurrency_semaphore.try_acquire_owned()`，**不阻塞**——满了直接回 `"Server busy: maximum concurrent task limit reached"`。上限来自 `THUNDER_MAX_CONCURRENT_TASKS`（默认 **8**，`.max(1)`，`service.rs:243-247`）。

随后依次：`task_id` 重入检查（`"Task '{id}' is already running (conflict)"`）→ session 独占（`stopping` 中的会话回专门的"仍在收尾"错误，否则"忙"；拒绝时**回滚**所有记账）→ `model_descriptor` 构建客户端（失败回滚，含 session 占用）→ 载入会话 → 模型选择优先级 `model` → descriptor 标签 → 会话记录 → 首个可用 → `"openai/gpt-4o"` → **workspace 首绑定不可变**（已有 `conversation.workspace` 胜出；只有 `shared_roots` 合并去重）→ thinking level 四级回退 → 模式（`.thunder/config.json` 的 `agent.defaultMode` → 否则 `Mode::default() = Ask`）→ 附件解析（失败即整请求报错）→ 模态检查（`model_supports_images`）→ 记录用户 turn + `store.save` + ack → `tokio::spawn` 运行体。

运行体（`service.rs:1426-1795`）：`AgentConfig::new(model).with_unlimited_turns()`、`request_timeout_ms = REQUEST_TIMEOUT_MS = 120_000`（`service.rs:24`，与 descriptor 客户端共用同一常量）、`session_id` 做 prompt-cache 亲和 → 组装 `ThunderRoot`（`.with_initial_session_total_tokens(会话累计用量)`）+ `StandardHostBuilder`（`.with_metrics_plugin(共享实例)`、`.with_status_plugin(共享实例)`）→ `!headless` 时 `.with_pack(ToolPack::new(ASK_USER_PACK_ID).with_tool(...))` → `RootRunOptions { route: task_id, ui: host_ui.scoped(task_id, session, status_plugin), mode_plugin, pause_gate, steer_queues }` → `root.execute(...)` → 事件泵（`TraceCollector::record` + 转发 `observed_event`，其中含插件派发与 `metrics_update` 快照 + `agent_status` 快照 + `SteerAccepted` 时补发队列快照 + `mode_changed` 的 custom 事件翻成顶层 `mode_changed` 帧）→ `handle.join()`（返回 `res.metrics`）→ 持久化（含 checkpoint 压缩时的 `save_raw_transcript`）→ 首轮/占位标题时后台生成标题 → 写 trace JSON（顶层 `metrics` 键 + `wall_duration_ms`/`session_total_tokens` 校正）→ 终帧 `task_completed` / `task_failed`（`finish_reason` 仅在确实存在一次 run 时携带）→ 清理五张表。

id 归属：daemon 的状态插件实例挂在 `DaemonService` 上（跨 run 共享），`scoped()` 除了 `task_id` 还带 `session_id` 与插件句柄——审批弹窗期间 `AwaitingGuard` 就是从这里报 `awaiting_user` 并保证四条出口都清干净（§11.8.2）。暂停同理：`pause_task`/`resume_task` 拿 `task_id` 去 `active_queues` 换 `session_id` 再报 `paused`，因为任务表和会话是两套 id。

### 12.4 并发与背压

| 机制 | 实现 |
|---|---|
| 准入 | `Semaphore`（默认 8），`try_acquire_owned` fail-fast；permit 移入 runner 全生命周期 |
| stdout 串行 | 单 `mpsc::channel::<String>(1024)` + 一个 writer task，攒到 **64 KiB** 或通道空时 `write_all` + `flush`（`service.rs:250-269`）；通道满则**await**（生产端阻塞而非丢帧） |
| 取消 | 每任务 `CancellationToken`；`cancel_task` 先移除再 cancel，所以二次取消回 `cancelled:false` |
| 暂停 | 每任务 `PauseGate`，只在**工具边界**生效 |
| 转向 | `active_queues: HashMap<task_id, LiveQueue>`，注入时用**该 run 的** workspace + roots 做附件 jail |
| 会话模式 | `SessionModes`：`HashMap` + `VecDeque`，容量 `MAX_SESSION_POLICIES = 256`，**FIFO 而非 LRU**（复用会话不会因此保命；LRU 会让"忙"永久钉住状态） |
| MCP 池 | 按 canonical workspace 路径键，上限 `MAX_WORKSPACE_MCP = 16`，淘汰时关连接但进行中的 run 仍持 `Arc` |

**已知阻塞点**：`handle_request` 被 stdin 循环**内联 await**（`main.rs:71`），所以 `generate_title`（真实 LLM 调用）与 `reload_integrations` 会阻塞其它请求受理（`run_task` 因为 ack 后即返回，不阻塞）。`[INFERENCE]`

### 12.5 双向交互（UI 反向 RPC）

**`ask_user_question`**（`ask_user.rs`）：id 由 daemon 铸造 `"{task_id}:q{seq}"`（`AtomicU64`，SeqCst），发 `user_question`，然后 `tokio::select!{ biased; 取消 → Err; timeout(1800s, rx) }`。答案渲染成 `Q: …\nA: …`；空对象 → `"User dismissed the question without answering."`；超时返回 Err 并**指导模型按已声明假设继续**。默认超时 **1800s**（是权限弹窗的 3 倍）。

**`HostUi` 弹窗**（`ui.rs`）：id 由 daemon 铸造 `"ui_{pid}_{seq}_{nanos}"`，超时取调用方 `timeout_ms`（否则 `DEFAULT_UI_TIMEOUT = 60s`），发 `ui_request`（flatten `UiRequest` 的内部 `ui` tag），**超时/通道关闭/发送失败一律 `UiResponse::Cancelled`**（fail-closed）。未知/过期 id 的答案被报告 `delivered:false` 且不生效——迟到的答案永远不会落到后来的弹窗上（`ui.rs:17-18,215-228`）。

附件（`attachments.rs`）：路径 jail = workspace ∪ shared roots ∪ **系统临时目录**（剪贴板粘贴），canonicalize + `starts_with`；base64 可带 `data:…;base64,` 前缀；**以嗅探出的 MIME 为准**；上限 `MAX_IMAGES_PER_MESSAGE = 10`、`MAX_IMAGE_BYTES = 5 MiB`（`thunder-agent-loop/src/core/images.rs:15-17`）。

---

## 13. 无头运行时：`thunder-agent-runtime`

二进制 `thunder-runtime`，协议版本 `"2"`（`runtime/src/session.rs:17`），**不持有任何产品状态**：模型、工具、完成判定都由宿主提供（`protocol.rs:1-6`）。

**请求**（tag = `method`，`protocol.rs:48-106`）：

| method | 字段 |
|---|---|
| `ping` | `id` |
| `start` | `run_id`, `model: ModelDescriptor`, `system_prompt?`, `messages[]`, `tools: RemoteToolSpec[]`, `max_turns?`, `temperature?`, `thinking_level?`, `request_timeout_ms?`（默认 60000）, `session_id?`, `gate?: {max_rounds}`（默认 3） |
| `tool_result` | `run_id`, `call_id`, `ok`, `output` |
| `gate_result` | `run_id`, `verdict`（`pass`/`retry`/`fail`）, `feedback?` |
| `cancel` | `run_id` |

字段普遍带 camelCase 别名（`runId`/`callId`/`maxTurns`/`thinkingLevel`/`requestTimeoutMs`/`sessionId`/`maxRounds`）。

**事件**（tag = `type`，`protocol.rs:112-158`）：`response`（带 `version`）、`observation`（原样转发 `AgentEvent`）、`tool_call`、`gate_request`、`run_finished`（带完整 `messages` + `stats` 供宿主持久化）、`error`。

`RemoteToolSpec.effect` 是可选的 `read|write|exec|plan_artifact`（`protocol.rs:18-21`）；`parse_effect`（`remote.rs:19-29`）映射到内核 `ToolEffect`，**未声明一律 `Other`（按变更处理）**。

`RemoteTool::execute` 把调用转成 `tool_call` 事件并在 oneshot 上等待（键为 `(run_id, call_id)`，且与 run 的取消令牌 `select!`）：宿主答 `ok:false` → `Err(output)`；宿主 drop 了 sender → `Err("the host dropped the tool call without answering")`；宿主进程没了 → `Err("the host is gone; tool call could not be delivered")`；run 被取消 → 立即返回并清理 pending。

`Runtime` 持 `runs: HashMap<run_id, CancellationToken>` 与两张 pending 表，所有输出经单个 `UnboundedSender<RuntimeEvent>` → 一个 writer task（一帧一行，不交错）；关闭时用 `watch` 通道让 writer 排空再退出（`session.rs:19-40`）。

---

## 14. 技能引擎：`thunder-agent-skills`

### 14.1 格式与解析

`Skill`（`types.rs:7-44`）：`name` / `description` / `version?` / `author?` / `tags[]` / `triggers[]` / `prompt_instructions` / `parameters?` / `location?` / `metadata{}`。

`SkillLoader::load_file`（`loader.rs:11-35`）按扩展名分派：`.json` → `parse_json`；`.yaml`/`.yml` → `parse_yaml`；**其余一律按 Markdown**（`SKILL.md` 走这条路）。

Markdown 解析（`parser.rs:25-90`）：以 `---` 开头时取 `\n---` 之前为 YAML frontmatter，`FrontmatterData` 的 `name` 缺省回退到文件名 stem，`description` 缺省回退到正文首行；`tags`/`triggers` 经 `extract_string_list` 支持标量或序列两种写法；**frontmatter 之后的正文就是 `prompt_instructions`**。

`load_dir`（`loader.rs:38+`）用显式栈做递归遍历（非递归 async），坏目录只 warn 不中断。

### 14.2 注册表与工具

`SkillRegistry`（`registry.rs`）：`Arc<RwLock<HashMap<String, Skill>>>`，`register` / `register_sync`（拿不到写锁就 spawn 一个任务）/ `unregister` / `get` / `contains` / `list` / `list_sync`；`from_dir` / `from_default_paths`。`list`、`list_sync`、`build_xml_catalog` 都按 **name 排序**输出——HashMap 的迭代顺序逐进程随机，而技能目录进的是系统提示前缀，顺序稳定是 prompt cache 命中的前提。`list_sync` 拿不到读锁时返回空表，需要"一条都不能漏"的调用方应使用 `list`。

`tools.rs` 把技能变成三个工具：`list_skills` / `load_skill` / `search_skills`（`lib.rs:19`）。

### 14.3 120s TTL 全局缓存

`thunder-agent-root/src/plugins/skills.rs:79-103`：

```rust
static DEFAULT_SKILLS_CACHE: OnceLock<RwLock<Option<(Instant, Vec<Skill>)>>> = ...;
// TTL 硬编码 Duration::from_secs(120)，读锁检查 + 写锁二次检查（double-check）
```

**为什么缓存**：`SkillsPlugin` 是基线常驻插件，而技能目录会被 host 初始化时扫描；逐次扫描会让 position 0 的提示块抖动并打穿 prompt cache。

### 14.4 作为常驻插件的形态

`SkillsPlugin` 是常驻 section：关键词选择已删除（见 §11.2），技能目录不再随提示词逐条切换，缓存前缀因此稳定。

`system_prompt_contribution`（`skills.rs:116-137`）把技能库渲染成一行一条的目录（每条描述**截到 100 字符**并加省略号），末尾提示"可用 `load_skill` 查看完整指令"——这是把大 playbook 挡在上下文之外、只暴露索引的设计。

`on_init` 的扫描顺序（`skills.rs:139-178`）：全局默认搜索路径（走 120s 缓存）→ 工作区 `<ws>/.agents/skills`、`<ws>/.pi/skills`、`<ws>/skills` → 显式 `search_paths`。

### 14.5 发现与加载的已核实细节

| 事实 | 证据 | 影响 |
|---|---|---|
| **先到先得，不是后到先得** | `load_search_paths` 用 `seen_names.insert(name)` 门禁压入（`loader.rs:106-129`）；而 `default_search_paths_from` 的注释写 "Order mirrors the pre-contract list (later wins)"（`loader.rs:161-162`） | 注释与实现相反。好在列表把**工作区**相对路径排在最前（`.agents/skills` → `.pi/skills` → `skills`，`:154-157`），先到先得恰好等于"工作区覆盖用户级"，与直觉一致 |
| **`node_modules` 会被遍历** | 目录跳过只有 `name_str != ".git" && name_str != "target"`（`loader.rs:70-74`） | 注释声称"Ignore .git, target, or top-level node_modules"（`:71`），但没有对应代码；技能目录下若有 `node_modules`，其中的 `.md` 都会被当技能解析 |
| **任何 `.md` 都是技能** | `load_dir` 按扩展名收 `.md/.markdown/.json/.yaml/.yml`（`loader.rs:81-86`），`load_file` 对 `.md` 一律走 `parse_markdown`（`loader.rs:30-34`） | `SKILL.md` 约定**不被强制**：技能目录里的 `README.md` 会变成一个名为 `README` 的技能并进入系统提示目录 |
| 前端 frontmatter 非必需 | `parse_markdown` 在无 `---` 时用文件名 stem 作 name、正文首行作 description（`parser.rs:42-54`） | 同上：一个普通说明文档能顺利"成为"技能 |

**120s TTL 不在本 crate**：README 把它记在 skills 上，实际实现位于 `thunder-agent-root/src/plugins/skills.rs:79-103`（`OnceLock<RwLock<Option<(Instant, Vec<Skill>)>>>`，**纯时间失效**，无 mtime/内容校验）。

---

## 15. MCP 客户端与服务端生态：`thunder-agent-mcp`

### 15.1 协议实现

`LATEST_PROTOCOL_VERSION = "2024-11-05"`（`protocol.rs:4`）。JSON-RPC 2.0 结构：`JsonRpcRequest` / `JsonRpcNotification` / `JsonRpcResponse` / `JsonRpcErrorDetail`（`protocol.rs:7-65`）。

已实现的方法与通知：
- 客户端（`client.rs`）：`initialize`（`:80`）→ `notifications/initialized`（`:110-111`）→ `tools/list`（`:128`）→ `tools/call`（`:165`）→ `ping`（`:196`）→ `close`（`:209`）。
- 进度通知（`protocol.rs` / `server.rs`）：`notifications/progress`（参数含 `progressToken`、`progress`、可选 `total` 与 `message`），供长时间运行工具向客户端回传流式进度。

### 15.2 stdio 传输

`StdioTransport::spawn`（`transport/stdio.rs:120-242`）：`Command::new(command).args(args)`，`stdin/stdout/stderr` 全 piped，`kill_on_drop(true)`（`:132`），注入 `config.env`，可设 `cwd`。spawn 出来的 `Child` **被存进结构体**（`child: Arc<Mutex<Option<Child>>>`，`:102`）——`kill_on_drop(true)` 使这个句柄成为 server 的生命周期：持有即存活，drop 即 SIGKILL。见 §15.2.1。

三个 task：写 stdin（`mpsc::channel(64)`）、读 stdout 按行解析并按 `id` 派发到 `pending_requests`（`PendingMap = Arc<StdMutex<HashMap<id, oneshot::Sender<JsonRpcResponse>>>>`，`stdio.rs:27-55`）、以及读 stderr 打日志。`is_alive: Arc<AtomicBool>` 让 manager 跳过死掉的 server。**每个请求都带时限**（默认 120s，可配置）且用 RAII 守卫注销，见 §15.2.2。

### 15.2.1 已修复：stdio server 曾被瞬间杀死

**曾有的缺陷**：`spawn` 对 `Command` 设了 `kill_on_drop(true)`，但 `Child` 只是一个局部变量——`stdin`/`stdout`/`stderr` 被 `.take()` 走后没有任何人持有它，于是函数返回时 `Child` 被 drop，tokio 立刻 SIGKILL 子进程。结构体当时只有 4 个字段（`server_name` / `stdin_tx` / `pending_requests` / `is_alive`），没有 child。

后果链：任何真实 stdio server 都完不成 `initialize`；`send_request` 要么立刻 `McpError::NotConnected`，要么等满 10 秒握手超时；`McpPlugin::on_init` 用 `if let Ok(cfg)` + `let _ =` 吞掉错误、fail-open 到 **0 个工具**（`root/src/plugins/mcp.rs:166-186`），所以故障是静默的。

**根因证据**（当时用 throwaway example 复现，同一 `kill_on_drop(true)` 下只改"是否留住 `Child`"）：

```
[control] child kept alive  -> pid 89020 alive = true     # Child 留着
[subject] pgrep 'sleep 5' right after spawn = ""          # 传输层 spawn 的进程在 t+0 就不存在
[subject] transport.is_alive()             = false        # stdout EOF ⇒ 读取任务把 is_alive 置 false
```

**修复**（`transport/stdio.rs`）：

1. `StdioTransport` 新增 `child: Arc<Mutex<Option<Child>>>`，`spawn` 末尾 `Some(child)` 存入；
2. `close()` 取出句柄并 `Child::kill().await`（发 SIGKILL **并 wait 回收**，返回时不是僵尸），幂等——二次调用是 no-op；
3. 已退出的进程让 `kill()` 返回 `Err`，只 `debug!` 记录，不算关闭失败；
4. 因为句柄现在真的被持有，`kill_on_drop(true)` 恢复了原本的语义：transport 及其所有克隆都被 drop 时，server 也会被关掉。

**回归测试**：新增 `thunder-agent-mcp/tests/stdio_lifecycle_test.rs`（真实 `sh` 子进程扮演 MCP server，非 `MockTransport`）：

| 测试 | 断言 |
|---|---|
| `a_real_stdio_server_survives_spawn_and_serves_requests` | 握手成功；`pgrep` 恰好 1 个 server 进程存活；`tools/list` 返回 1 个工具；`tools/call` 返回 `pong`；连续 `ping` 仍可用；`close()` 后 `is_alive()==false` 且 `pgrep` 归零 |
| `a_configured_server_yields_bridged_tools` | `McpManager::from_config` → `list_servers()` == `["fake"]` → `discover_all_tools()` 得到 `mcp_fake_echo` → `close_all()` 后进程归零 |

`printf` 输出与 `pgrep` 断言让"进程真的活着/真的没了"成为可观测量，而不是只看一个布尔标志。A/B 复核：**原源码下两个测试均失败**（`initialize handshake timed out after 10s`），**修复后 0.11 s 通过**；`cargo fmt --check`、`cargo clippy -- -D warnings`、`./test.sh` 全工作区通过。

### 15.2.2 已修复：请求无超时 + pending 表泄漏

**曾有的缺陷**（与 §15.2.1 同批修掉）：

- `send_request` 在 `rx.await` 上**没有超时**（旧 `stdio.rs:177-182`）：一个只读不回或干脆挂住的 server，会让 `tools/call` 永远等待。
- pending 表只在"发送失败"这一条路径上 `remove`（旧 `:168-184`）；序列化失败的 `?` 早退与（后来才有的）超时路径都不移除 → **每次超时留一条永久 entry**。
- 顺带查出：`McpServerConfig::timeout_ms` 是**死字段**（`config.rs:18` 声明、`:53` 设置，全仓无消费者），所以"已经配了超时"这件事本身也不成立。

**修复**（`transport/stdio.rs`）：

1. `PendingMap` 包装该表（`insert` / `take` / `len` / `clear`），用 `std::sync::Mutex`——表只在同步片段内被持有，从不在 `await` 期间跨持。
2. `PendingEntry` 是 RAII 守卫：`register` 插入，`Drop` 注销。**早退 `return`、`?`、超时、成功**四条出口自动覆盖；成功路径上读取任务已先 `take`，`remove` 幂等，二者不会互踩。
3. `send_request` 用 `tokio::time::timeout(limit, rx)` 包住等待，超时报 `MCP server '<name>' did not answer request '<id>' within <ms>ms`。**超时 ≠ 死亡**：`is_alive` 不变，transport 继续可用。
4. `close()` 同时 `pending_requests.clear()`——server 已走，不会再有回复，丢掉 sender 让每个 waiter 立刻以 "channel dropped" 解开，而不是挂到进程结束。
5. `McpServerConfig::timeout_ms` 接上了：`None` → `DEFAULT_REQUEST_TIMEOUT = 120s`；`Some(0)` → **显式放弃时限**（给合法无上限的工具留口）；`Some(ms)` → 毫秒。
6. 新增诊断接口 `StdioTransport::pending_len()`：静置为 0，只涨不回说明回复在丢。

**回归测试**（追加进 `stdio_lifecycle_test.rs`，server 是"只读不答"的 `sh` 脚本）：

| 测试 | 断言 |
|---|---|
| `a_silent_server_times_out_and_leaves_no_pending_entry` | `timeout_ms=300` 时调用在 5s 内返回 `Err` 且错误含 `did not answer`；`pending_len()==0`；`is_alive()` 仍为真（慢 ≠ 死） |
| `an_explicit_zero_timeout_opts_out_of_the_bound` | `timeout_ms=0` 时 400ms 后请求**仍在飞**（`pending_len()==1`、任务未结束）；`close()` 后 waiter 立刻解开且 `pending_len()==0` |

**为什么没有"修复前失败"的 A/B**：修复前这个测试**不会失败，会挂住**——缺陷本身就是"没有上界"，没有可失败的断言点。旧行为用静态证据钉住：`send_request` 全函数无 `timeout`，且 `timeout_ms` 无消费者（`grep` 可复核）。

### 15.2.3 其它已核实缺口

| 缺口 | 证据 | 影响 |
|---|---|---|
| server→client 请求被丢弃 | 读取任务只 `serde_json::from_str::<JsonRpcResponse>`（`stdio.rs:202`），未知 id 只 `debug!` 后忽略（`:211-213`） | sampling / roots / elicitation 全部不可用 |
| 图片内容被降级为占位符 | `CallToolResult::plain_text` 把 `ToolContent::Image` 渲染成 `[Image: mime]`（`protocol.rs:184-188`），base64 被丢 | MCP 返回的图片永远到不了模型 |
| 桥接工具效果分类靠猜名字 | `McpToolBridge` **没有**覆写 `effect()`（`tool_bridge.rs`），落到 `ToolEffect::of("mcp_x_y")` | 只有名字里含 `exec`/`shell`/`terminal`/`powershell`/`command`，或以 `_bash` 结尾的才是 `Exec`（`loop/types/policy.rs:68-80`），其余按 `Other`（≈`Write`）判——再叠加 `CodeScope` 对 `Other` 返回 `Unknown`，实际退化为"每次问" |

（`thunder-agent-mcp/tests/mcp_test.rs` 断言的是 `MockTransport` 往返，不覆盖以上任何一条。）

### 15.2.4 帧上限：在读取过程中中止

`McpServerConfig::max_frame_bytes`（默认 `DEFAULT_MAX_FRAME_BYTES` = 4 MiB）。读取任务不用 `BufReader::lines()`，而是 `read_frame`：逐段消费，累计到上限后**继续读但不再追加**，返回 `Frame::Oversized { bytes }`。

超限帧的 id 在被丢弃的部分里，无法配对，所以读取任务直接 `pending_requests.clear()`：所有在途请求立刻拿到 "channel dropped" 错误，而不是各自等完 120 秒。连接不会因此报废，下一帧照常读取。`StdioTransport::oversized_frames()` 给出计数。

回归测试 `stdio_lifecycle_test.rs::an_oversized_reply_is_discarded_and_fails_the_call` 用真实子进程回 10 MiB，断言"5 秒内失败 + 计数为 1"。

### 15.3 配置发现顺序

`McpConfig::find_and_load_from_workspace`（`config.rs:127-161`）依次尝试：

```
<ws>/.thunder/mcp.json → <ws>/mcp_servers.json → <ws>/.mcp.json → <ws>/claude_desktop_config.json
→ ./mcp_servers.json → ./.mcp.json
→ <数据根>/mcp.json
→ ~/.mcp.json → ~/mcp_servers.json → ~/.cursor/mcp.json → ~/.claude.json
→ ~/Library/Application Support/Claude/claude_desktop_config.json → ~/.config/claude/claude_desktop_config.json
```

**第一个存在且 `mcpServers` 非空的胜出**。thunder 自己的 `mcp.json` 跟随数据根，其余是用户的跨工具文件，仍取真实 HOME（`config.rs:137-139`）。解析兼容两种形状：`{"mcpServers": {…}}` 与裸的 `{"server": {…}}` map（`config.rs:85-98`）。

### 15.4 工具与插件双模桥接

`McpToolBridge`（`tool_bridge.rs`）同时实现了 `AgentTool` 与 `ToolPlugin` 双重特征契约：
- **作为 `AgentTool`**：可被直接注册至 AgentLoop 或工具注册表，暴露命名为：
  ```
  mcp_<server_name 中 - 换 _>_<tool_name 中 - 换 _>
  ```
  描述前缀 `[MCP:<server>] `，参数 schema 直接透传 `input_schema`。
- **作为 `ToolPlugin`**：在洋葱拦截器流水线执行时可接收富上下文 `ToolExecutionContext`，支持调用递归调用代理 `ctx.invoke` 以及流式报告进度 `ctx.report_progress`。

`McpManager::discover_all_tools_timeout`（`manager.rs:93-127`）**每 server 10 秒超时**，超时/报错的 server 只 warn 并跳过，防止一个挂死的 server 阻塞整个 run；`is_alive() == false` 的 server 直接跳过。`close_all` 逐个 `close()`（`manager.rs:156-162`）。

### 15.5 MCP 服务端：`McpServer` 与流式进度推送

`thunder-agent-mcp` 提供完整的 MCP 服务端抽象（`server.rs`），能够将任何本地 `ToolPlugin` 集合以标准 MCP Server 协议向外部宿主（Claude Desktop、Cursor、VS Code 或其它 Agent）暴露：

- **基础能力**：`McpServer::new(name, version)` 配合 `with_tool` / `with_tools` 注册任意 `Arc<dyn ToolPlugin>`；
- **传输服务**：`server.serve(reader, writer)` 提供标准 stdio 流式事件循环，按行处理 JSON-RPC 2.0 请求（`initialize`、`notifications/initialized`、`tools/list`、`tools/call`、`ping`）；
- **进度中继**：当外部调用传入 `_meta.progressToken` 时，服务端自动构造 `McpStreamProgressReporter` 并通过 `ToolExecutionContext` 传递给插件；插件调用 `ctx.report_progress(...)` 时，以异步 JSON-RPC `notifications/progress` 消息通过 stdout 实时流式通知客户端。

---

## 16. 会话存储：`thunder-conversation`

### 16.1 数据模型

```rust
pub struct Conversation {
    id, title: Option<String>, title_source: Option<String>,   // "auto" | "manual"
    parent_id, system_prompt, model, workspace, shared_roots: Vec<String>,
    thinking_level, status: ConversationStatus,
    messages: Vec<ChatMessage>,
    stages: Vec<StageRecord>, orchestration: Option<OrchestrationMeta>,
    metadata: HashMap<String, serde_json::Value>,
    stats: ConversationStats, created_at_ms, updated_at_ms,
}
```
（`types.rs:79-112`）

`ConversationStats`（`types.rs:20-34`）区分两个 token 量：`total_tokens`（**当前工作上下文的估算**，每次变更重算）与 `total_used_tokens`（**累计 provider 上报用量，含 cache 读写**，只做加法，从不从文本重算）。

**一轮 = 一条 assistant 消息**（`types.rs:262-280` 的 `add_assistant_turn`）：消息携带该轮 `content`（这一轮流出来的正文）、`tool_calls`（本轮发出的全部调用）、`reasoning`（该轮的思维链）与 `stats: Option<TurnStats>`（引擎轮次号、token、耗时）。最后两个字段是**宿主才有的信息**：`ChatMessage::Assistant` 的 `reasoning`/`stats` 在引擎侧恒为 `None`（`engine.rs:1020-1035`：那是请求上下文里的消息，不是转录记录），只有落盘这一侧才附着——provider 转换只读 `content`/`tool_calls`（`providers/client.rs:263-283`），因此这两个字段永远不会出现在请求里。

理由是可读性与可恢复性：一轮的正文与思维链是**流式 delta**、调用与结果是**交错到达**的，`turn_end` 之前没有"一轮"这个东西（`ConversationPlugin` 的 `PendingTurn` 缓冲，`conversation.rs:32-90`）；而思维链除了这里**没有任何别的去处**——provider 只推一次，trace 收集器把它当流式噪声计数后丢弃（`trace_collector.rs:48-58`），转录不留它就永久丢失。落盘顺序固定为「本轮 assistant 消息 → 本轮工具结果」，也正是 provider 读回时期望的顺序（一条 assistant 带 N 个 tool_call，后跟 N 条 tool 结果）。

### 16.2 持久化门禁（本 crate 最硬的规则）

```rust
async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
    if !conversation.has_user_turns() { return Ok(()); }   // 不落盘，连目录都不建
    ...
}
```
（`store/fs.rs:286-315`、`store/mod.rs:13-20`）

`has_user_turns`（`types.rs:215-219`）**从消息列表推导，而非 `stats.turn_count`**——后者只在 `touch()` 时刷新，可能滞后。宿主可以先塞 system prompt 和一句 assistant 问候，但"还没人对它说过话的会话不是 session"。

### 16.3 磁盘布局与写入协议

```
<root>/                                  root = thunder_subdir("conversations")
  index.json                             HashMap<String, ConversationSummary>
  <conversation-id>/
    conversation.json
    raw_transcript.jsonl                 仅在发生 checkpoint 压缩时
    traces/<task_id>.json                由宿主写入（本 crate 不写）
```
（`store/fs.rs:59-69,128-138`）

每次写都是 **tmp + rename**，tmp 名内嵌 pid + `now_ms()` 保证并发写者不互相踩（`fs.rs:261-263`）。

`persist_index` 是**读-合并-写**（`fs.rs:264-281`）：

1. 重读磁盘 `index.json`（损坏/不可读 → 退化为空 map，只 warn）；
2. `merge_index_maps`：同 id 冲突时 `updated_at_ms` 新的胜出；
3. `prune_missing_dirs`：`conversation.json` 已消失的行删除（**stat 结果不确定时保留该行**——绝不因瞬时文件系统错误销毁条目）；
4. 原子改名写回。

这解决的是经典问题：TUI 与 daemon 共享同一 root，后来的写者若整体覆盖就会让另一进程新建的会话在 `list` 里消失。

`FsConversationStore::new` 对 `ErrorKind::NotFound` **重试 10 次、间隔 10ms**（`fs.rs:19-44`）——`create_dir_all` + `read_dir` 不是原子的，多进程同时为一个全新 root 建 store 时，输家可能观察到瞬时 NotFound。

### 16.4 多拓扑阶段追踪

`OrchestrationTopology { Sequential, Parallel, Delegate }`（`orchestration.rs:5-19`）。`OrchestrationHelper` 提供：

| 函数 | 语义 |
|---|---|
| `create_branch(parent, branch_id, topology, role, agent_id, run_id)` | 派生分支会话，带 `OrchestrationMeta` 与 `parent_id` |
| `record_sequential_stage(conv, role, brief, &AgentRunResult)` | Planner→Coder 式的阶段记录 |
| `merge_parallel_into_parent(parent, run_id, &[(String, AgentRunResult)])` | 生成 `### Parallel Multi-Agent Execution Summary` markdown，每分支一节 + 一条 `StageRecord`，再作为一条 assistant 消息追加 |
| `record_delegated_task(parent, tool_call_id, tool_name, sub_conv_id, &AgentRunResult)` | 委派子会话 |

`StageRecord`（`types.rs:66-77`）记录 `stage_id`（`stage_{n}_{role}`）、`role`、`agent_id`、`task_brief`、`final_content`、turn/tool 计数、时长、`finish_reason`、时间戳。

### 16.5 标题

`provisional_title(prompt)`：`PROVISIONAL_TITLE_CHARS = 30` + **三个 ASCII 点**（`types.rs:640-656`）——刻意用 ASCII 省略号，好让 `is_title_placeholder` 的 `ends_with("...") && len >= 3` 生效。`add_user_message` 另有一条**40 字符且不加省略号**的自动标题路径（`types.rs:221-234`），两者是不同常量。

`is_title_placeholder`（`types.rs:151-162`）：`None` / 空白 / `"new conversation"` / `"untitled"` / 以 `...` 结尾且长度 ≥ 3 的都算可替换。

### 16.6 已知细节

- `compact_history` 是**有损且不调用模型**的：保留 system prompt、插入一句"已裁剪 N 条，磁盘文件为准"的提示、保留最后 N 条（`types.rs`）。窗口边界会向前回退，避免以孤立的 tool result 开头（provider 会拒绝）；早先版本插入的固定"摘要"是**虚构内容**（没有任何模型读过被删的对话），已删除。
- 同一 crate 内共存两个 token 估算器：`recalculate_stats` 用 `ContextBuffer::calculate_message_tokens`，`generate_stats_report` 用 `char_count / 3`（`types.rs:471`）。图片按 `mime_type.len() + 24` 计，刻意不数 base64（`types.rs:454-458`）。
- `fork` 设 `title_source = "auto"`，尽管它自己铸造了 `(Fork)` 标题——自动命名器可能覆盖这个标签。`[INFERENCE]`
- `ConversationStatus::{Deleted, Paused}` 存在但无人设置：`delete` 直接删行，`archive` 是唯一改 status 的入口。
- `metadata["tags"]` 是本 crate 唯一会读的 metadata 键（`types.rs:363-372`），用于过滤。

---

## 17. 终端界面：`thunder-tui`

### 17.1 形态

Ratatui 全宽终端，单事件通道驱动的状态机。`App` 有 74 个 pub 字段（`app.rs:130-287`），核心分组：会话/输入编辑器/运行控制/追踪/渲染开关。

两个执行拓扑（`ExecutionMode`，`app.rs:43-47`，默认 `AutoRouter`）：

| 拓扑 | 路径 |
|---|---|
| `AutoRouter` | `ThunderRoot` + `StandardHostBuilder`（微内核宿主，插件路由） |
| `SingleAgent` | 直接 `AgentLoop` + `CodeScope` + 模式插件作 gate，注册 6 个内建工具 + 模式插件自带工具 |

### 17.2 事件与渲染循环

```
终端线程（阻塞 read）─┐
   tick 任务（50ms）  ─┼─► mpsc::unbounded_channel::<AppEvent>() ─► TuiRunner::run
   agent 事件中继     ─┘
```

`TuiRunner::run`（`runner.rs:30-116`）：

1. `app.reconcile_pane_status()`——把宿主侧的两个等待与插件快照折成下一条窗格状态（§17.4）；
2. `needs_redraw` 为真才 `terminal.draw(...)`；
3. 有 pending 就往外写一条 OSC 7501（**在 draw 之后、且不受 `needs_redraw` 约束**：它面向终端模拟器而非屏幕缓冲，runner 这里是单线程的，不会和 ratatui 的半帧绘制交错）；
4. `events.next().await`；`None`（所有 sender 都没了）→ 退出；
5. `dispatch_event`；
6. **再 `try_next()` 批量排空最多 32 个事件**——这就是流式 token 的批处理；
7. `should_quit` → 保存会话后退出（退出前补一条 `state=idle`，§17.4）。

终端线程是**独立的 OS 线程**（`event.rs:68-104`），阻塞在 `crossterm::event::read()`，所以既不占 tokio worker，也能在按键落下的瞬间被读到；`Interrupted` 重试，其它错误终止。鼠标 `Moved` 事件在读取线程被**合并**（前一个未处理则丢弃），在 `handle_mouse` 里再次只在悬停目标变化时标脏——这是 AnyEvent 鼠标追踪下的已知 CPU 大户。

空闲时 `is_animating()` 为假（未运行、未暂停、无排队、无新鲜状态），所以不重绘；`TranscriptKey` 在空闲时把 spinner 计数清零，好让缓存命中（`chat.rs:186-192`）。

### 17.3 输入优先级阶梯

`handle_key`（`app.rs:1588-1795`）顺序固定：

```
0. pending_question  → handle_question_key（agent 被阻塞在这里）
1. picker.is_open    → picker.handle_key → handle_picker_selection
2. mode == Help      → 自己的滚动键；只有全局键与 Esc 能穿透
3. 全局键：Ctrl+C 中断 / Ctrl+Q 退出 / Ctrl+N 新会话（run 进行中会被拒绝） / Ctrl+P 切拓扑 /
   Ctrl+B 侧栏 / Ctrl+O 展开详情 / Ctrl+L 链接选择器 / Ctrl+H 帮助 /
   Ctrl+T 时间轴 / PageUp·Down ±10 行 / Home·End / Shift·Alt+↑↓ ±1 行 /
   Tab（参数轮转 → 命令补全 → 焦点循环）/ BackTab（参数反向 → 模式轮转）/
   Esc（改名 → 监视焦点 → 选区 → slash 输入 → stop_run）
4. 按焦点分派：handle_input_key / handle_chat_scroll / handle_sidebar_key / handle_monitor_key
```

Enter 的语义（`app.rs:1814-1868`）：`/` 开头先试补全；**运行中 Enter = `Steer`，Alt+Enter = `FollowUp`**；空闲时提交。

### 17.4 一次 run 的完整链路

`submit_prompt`（`app.rs:3988-4092`）：剥离 `/single`、`/auto` 前缀 → 折叠 `pending_images` 进用户 turn → 状态 `Thinking` + `clear_run_state` + `auto_scroll` → 把 model/workspace/thinking/shared_roots 绑到会话 → 判断首轮 → 首轮且标题占位时写 `provisional_title` → `begin_trace(prompt)`（`task_id = run_<now_ms>`）→ `save_and_refresh()` → 新建 `CancellationToken` / `PauseGate` / `SteerQueues` / `metrics.begin_run()` → 分派到 `run_single_agent` 或 `run_root_agent`。

`run_root_agent`（`app.rs`）构造 `AgentConfig`（`with_unlimited_turns`、`session_id = conversation.id` 做缓存亲和、`pruning.max_context_tokens = spec.context_window`、`prompt_cache_warm`），包进 `StandardHostBuilder`（带 `.with_status_plugin(App 自己的实例)`，见下），`/ask on` 时 `.with_pack(ToolPack::new(ASK_USER_PACK_ID).with_tool(TuiAskUserTool::new(...)))`，然后 `RootRunOptions { route: run_id, ui: TuiHostUi, mode_plugin, pause_gate, steer_queues }`。

**route 就是 trace 的 task_id**（`app.rs:4154-4160` 注释）：一次 run 两个名字会导致文件里的 `task_id` 与文件名不符。

**窗格状态：OSC 7501**（`pane_status.rs`）：把「这个运行在干什么」写给**终端**。宿主面板（agent-resume-panel 的 `cli:thunder` 窗格）只看得见 pty 上的字节，OSC 7501 正是它扫的 Program Status Protocol——因此不需要 socket、不需要握手、也不需要面板配合。

- **读快照，不解析事件**：`App::reconcile_pane_status` 每帧从 `self.status_plugin`（与 root 共用的那个实例）取 `snapshot()`，顺带用 `note_awaiting` / `note_paused` 补上两个只有终端知道的等待。**不建第二条状态机**是硬要求；`/single` 直连路径没有插件宿主，由 `begin_run` + `record_session_event` 手工喂养同一个实例，所以两种执行模式报的是同一份状态（这也是 `handle_agent_event` 里没有 `agent_status` 分支的原因：那条 `Custom` 帧只有其他宿主消费）。
- **只在报告变了时发**（`PaneStatus` 记着上一条正文）：同一阶段内的 `turn` 变化不发，否则 pty 上全是噪声。
- **协议的三条硬约束**：不带 `id=`（带 id 的记录算子记录，不拥有 pane）；`state` 是闭集 `working|blocked|error|idle|done|clear`（未知值整条丢弃）；键 `[a-z]{1,16}`、值 `[A-Za-z0-9_.,+/=-]*`、单条 ≤ 4096 字节。所以工具名不合法时**只丢 `kind` 对**，不废掉整条报告（MCP 工具名带空格/括号/冒号很常见）。
- **从第一帧就开始报，而不是从第一次 run 开始**：一条 `idle` 报告同时也是**给窗格命名**的动作——面板是从报告本身知道这个窗格里跑的是哪个程序的（`app=thunder`），沉默到有活干为止的 TUI 在那之前是个匿名窗格。
- **退出补一条 `idle`**：窗格活得比进程久，而一旦某个程序报过状态，面板就**不再读该窗格的屏幕**了——不撤记录的话，退出后接管的 shell 会一直被当成「工作中」。从没报过状态的 `PaneStatus` 保持沉默：报告本身就是"改权威"的动作，不是装饰（对自己直接跑的终端无害，对面板是有意义的声明）。
- **不发探测序列** `ESC ] 7501 ; ?`：面板的回执会写进 pty 的 stdin，被 TUI 当成键盘输入。

`handle_agent_event`（`app.rs:4432-4615`）逐事件映射：

| 事件 | 宿主动作 |
|---|---|
| `TurnStart` | `Thinking`，重置 `open_assistant`/`pending_thinking` |
| `TokenDelta` | `Streaming`，累加 `streaming_delta`，记 live segment |
| `ReasoningDelta` | 累加 `reasoning_delta`，记 live segment |
| `SteerAccepted` | 从宿主自己的 `queued_turns` 找回附件，写入 transcript，刷新队列快照 |
| `ToolCallReady` | `attach_tool_call`（折进本轮 assistant 消息）+ 记 `ActiveToolCall` |
| `ToolExecStart` | `ExecutingTool { name }` |
| `ToolExecResult` | 更新活动调用、`has_tool_result` 去重后写 tool 消息、记 `ToolOutcome` |
| `TurnEnd` | 记 metrics、把 thinking 存进 `run_thinking`（每轮一条）、写 assistant 消息 |
| `Error` | `last_error` + `Error` 状态 |
| `ContextCompacted` | 仅状态提示 |
| `Custom { kind: "metrics_update" }` | 反序列化 `MetricsSnapshot` 直接刷新遥测条（`Metrics::apply_snapshot`）——速率、上下文占用、累计消耗一律以它为准（本地计数只在没有快照时兜底，例如 `/single` 直连路径不建 `ThunderRoot`） |
`handle_agent_finished`（`app.rs:4616-4777`）三分支：取消（保留 partial answer、无错误）、成功（采纳引擎权威消息 + `reattach_run_thinking`）、失败（合成 `❌ …` 消息）。随后无条件清理运行态、`metrics.finish_run`、**加法式**折 `run_stats` 进会话统计、`finalize_trace`。

**thinking 的重新附着**（`app.rs:968-1066,4872-4883`）：引擎的权威消息列表**不带 reasoning**，所以宿主用 assistant 消息的"身份键"（`call:{首个 tool_call_id}` 或 `text:{内容}`）索引 thinking；权威替换后按位置**从尾部对齐**配对，因为引擎重建的正文可能有一个字节的漂移。

### 17.5 渲染模块

| 模块 | 行 | 职责 |
|---|---|---|
| `ui/mod.rs` | 115 | 唯一裁帧处：先问 `footer_height`/`metrics_height`/`input_box_height`，再切 5 段垂直布局；叠加顺序 = 轨道 → footer → picker → question → help |
| `ui/chat.rs` | 941 | transcript：`TranscriptKey` 命中才重建；`Stream` 维护行 + 屏幕行映射 + 链接 span + turn 标记；每帧重发链接 hitbox |
| `ui/markdown.rs` | 1257 | 自研**宽松**渲染器：块级（围栏/ATX 标题/分隔线/引用/表格/列表/段落）+ 行内 `Frag`；**每行恰好一个屏幕行**（换行在这里做，绝不交给 `Paragraph`）；表格列宽上限 40 格 |
| `ui/timeline.rs` | 288 | 右侧悬浮 turn 轨道：宽度 < 100 列时不画；高亮窗口 `rail_window`；悬停浮出单行提示；每帧重发 hitbox |
| `ui/metrics.rs` | 1022 | 遥测条：9 类 segment 按重要性贪心打包（**绝不拆开一个 segment**），`bar_height` 用同一算法量高度；上下文占用 ≥ 80% 变红。速率 / 上下文占用 / 累计消耗优先取 `MetricsPlugin` 的 `metrics_update`（`Metrics::apply_snapshot`，轮内也在前进），本地计数只在无快照时兜底 |
| `ui/status_bar.rs` | 469 | 提示框 + 底栏：输入按显式换行与软换行切行（不切宽字符），上限 `MAX_INPUT_ROWS = 6`；底栏**永远画会话行**，瞬时状态叠在它上面 |
| `ui/help.rs` | 553 | 帮助弹窗：`help_lines` 静态表与渲染分离，好让 `help_max_scroll` 用同一段落量高度 |
| `ui/command_popup.rs` | 241 | 斜杠命令补全：命令名补全后若该命令有固定首参，切换为参数候选渲染；上限 8 条 |
| `ui/picker_modal.rs` | 233 | 通用下拉选择器（会话/模型/模式/技能/MCP/trace/链接） |
| `ui/question_modal.rs` | 192 | `ask_user_question` 弹窗：队列感知（标题显示 `已答+1 / 总数`），多选用 `◉/○`，单选 `▶` |
| `ui/header.rs` | 99 | 单行：品牌 + 模型 + 拓扑徽章 + 技能/思考档位 chip + 状态（或 `⏸ PAUSED`） |
| `ui/sidebar.rs` | 105 | 会话列表——**`draw` 从不调用它**（`ui/mod.rs:60-63` 全宽画 transcript）；侧栏开关目前只影响焦点循环 |
| `ui/theme.rs` | 70 | 13 个 `pub Color` 字段，唯一决定颜色的地方 |

### 17.6 斜杠命令（`ALL_COMMANDS`，31 条）

`commands.rs:69-302` 的表（`name` | 别名 | 参数）：

| # | 命令 | 别名 | 参数 |
|---|---|---|---|
| 1 | `resume` | `load_session`,`switch`,`sessions` | `[session_id \| #]` |
| 2 | `help` | `?` | — |
| 3 | `model` | `m` | `[provider/model]` |
| 4 | `topology` | — | `[auto \| single]` |
| 5 | `mode` | — | `[plan \| ask \| acceptEdits \| yolo]` |
| 6 | `think` | `thinking` | `[off \| low \| medium \| high]` |
| 7 | `roots` | `root`,`shared` | `[add <path> \| remove <p\|#> \| clear]` |
| 8 | `ask` | `ask_user` | `[on \| off]` |
| 9 | `pause` | `hold` | — |
| 10 | `unpause` | `go`,`continue` | — |
| 11 | `image` | `attach`,`img` | `<path> [path...]` |
| 12 | `skills` | `skill`,`sk` | `[attach \| load <name> \| show <name> \| off]` |
| 13 | `mcp` | `server`,`tools` | `[list \| servers \| reload]` |
| 14 | `trace` | `traces` | `[list \| <task_id>]` |
| 15 | `rename` | — | `[name \| --auto]` |
| 16 | `prompt` | `prompts`,`template` | `[list \| show <name> \| run <name> [args]]` |
| 17 | `memory` | `mem` | `[list \| show <file>]` |
| 18 | `clear` | `new`,`reset` | — |
| 19 | `config` | `settings`,`cfg` | `[key] [value]` |
| 20 | `compact` | `prune`,`compress` | — |
| 21 | `stats` | `cost`,`tokens`,`usage` | — |
| 22 | `workspace` | `cwd`,`dir` | `[path]` |
| 23 | `export` | `save`,`dump` | `[path]` |
| 24 | `preview` | `md`,`rich` | `[on \| off]` |
| 25 | `links` | `urls`,`files` | — |
| 26 | `queue` | `queued`,`pending` | `[clear \| all \| one]` |
| 27 | `metrics` | `meters` | `[on \| off]` |
| 28 | `timeline` | `rail` | `[on \| off]` |
| 29 | `details` | `verbose` | `[on \| off]` |
| 30 | `health` | `doctor`,`status` | — |
| 31 | `quit` | `exit`,`q` | — |

分类：General / Config / SkillsAndMcp / Session（`commands.rs:2-7`）。`filter_commands` 对首个空白分隔 token 做**前缀**匹配（名字或别名，大小写不敏感）；参数轮转只改**第一个**参数，按字节区间原地替换，后面的自由文本保留。

### 17.7 CLI 与默认值

`--mock` **被显式拒绝**并 `exit(2)`（`main.rs:15-21`）——不能让操作者以为在跑假模型而实际在用真模型；测试缝是 `ClientFactory`。`--workspace <dir>`（非目录 → `exit(2)`）、`--session <id>`、`MODEL`、`THUNDER_WORKSPACE`、`RUST_LOG`。日志写 `<数据根>/logs/tui.log`（默认过滤 `warn`），**打不开日志不影响会话运行**（fail-open）。

默认值（`App::new`）：`temperature = 0.2`、`max_turns = 30`、`request_timeout_ms = 120_000`、tick 50ms、`execution_mode = AutoRouter`、`markdown_enabled = true`、`timeline_visible = true`、`ask_user_enabled = true`、`metrics.enabled = true`、`details_expanded = false`。

`max_turns` 现在真正进入 `AgentConfig`（两条执行路径都用 `with_max_turns`），且 `/config max_turns 0` 被拒绝——引擎在首轮之前就检查 `turn >= max`，`0` 会让每次 run 立即结束。项目级 `.thunder/config.json` 的 `agent.maxTurns` 仍可覆盖它（root 在此之后应用该层）。

---

## 18. 横切关注点

### 18.1 失败方向总表

| 位置 | 方向 | 表现 |
|---|---|---|
| 内核 guard | **fail-closed** | 危险命令 / Critical 路径一律拒，无模式可放宽 |
| 内核 gate 缺席 | **fail-open** | 除 guard 外全放行（无头宿主的契约） |
| 内核弹窗 | **fail-closed** | 无面板/超时/取消/非法答案 → 拒绝 |
| 内核 `ToolScope` 缺席 | **fail-safe** | 一律 `Unknown` → 不赚自动放行 → 变成"问" |
| pack `CodeScope` | **fail-open** | 无法判断 → `Unknown`，绝不猜 |
| root `.thunder/config.json` 损坏 | fail-open | warn 后跳过 |
| root `systemPromptFile` 读不到 | fail-open | 仅 warn |
| root `on_finish` 报错 | fail-open | `let _ =` 吞掉 |
| root 模型解析失败 | fail-open | 留下 `UnconfiguredLLMClient`（下次调用报错） |
| root plan 模式未获批准 | fail-closed | 停在 plan 并返回 `Ok`（不告诉模型重试） |
| daemon UI 弹窗 | **fail-closed** | 超时/关通道/发送失败 → `Cancelled` |
| daemon 附件校验失败 | fail-closed | 整请求报错 + 记账回滚 |
| daemon 未知模式 | fail-closed | 报错并列出合法集合，不回落 |
| conversation 索引损坏 | fail-open | warn + 重建索引 |
| TUI 日志打不开 | fail-open | 会话照跑 |
| TUI 无模型/工作区非法 | fail-closed | `exit(1)` / `exit(2)` |
| TUI 无 LLM 客户端 | fail-closed | 发失败 `AgentFinished` 而不是静默用未配置客户端 |

### 18.2 常量与硬编码限制总表

| 值 | 位置 | 含义 |
|---|---|---|
| `50` | `loop/types/config.rs:131` | 默认 `max_turns` |
| `128_000` | `loop/types/config.rs:48` | 默认 `max_context_tokens` |
| `16_384` | `loop/types/config.rs:33-35` | `reserve_tokens` |
| `20_000` | `loop/types/config.rs:37-39` | `keep_recent_tokens` |
| `4096` | `loop/types/config.rs:41-43` | `summarizer_max_tokens` |
| `3` | `loop/engine.rs:124` | `max_gate_rounds` |
| `512` | `loop/engine.rs:316` | 每 run 事件通道深度 |
| `128` | `loop/hooks.rs:13` | broadcast 旁路容量 |
| `2` | `loop/engine.rs:616` | 上下文溢出自动重试次数 |
| `2` | `loop/types/config.rs:135` | `max_stream_retries` |
| `600s` | `loop/tools/executor.rs:31` | 审批弹窗默认超时 |
| `2048` | `loop/tools/executor.rs:36` | trace 输出预览字符数 |
| `64KB` / `30s` | `loop/tools/registry.rs:24` | 默认输出上限 / 默认超时 |
| `16KB` / `1KB` | `loop/tools/scratchpad.rs:53-54` | 落盘阈值 / 预览字节 |
| `1600` | `loop/core/token_estimator.rs:7` | 单图 token 估算 |
| `2000` / `200` | `loop/pruning/checkpoint.rs:27-28` | 摘要输入的 tool result / args 截断 |
| `0.15` / `$0.05` / `30min` | `loop/cache/warmer.rs:39-46` | 空转概率 / 最小预期节省 / 空转年龄上限 |
| `10` / `3` / `5` / `5` | `loop/types/config.rs:68-77` | LoopGuard 四阈值 |
| `5s` | `loop/engine.rs:1455` | 结构化事件投递超时 |
| `10MB` | `pack-code/builtin/fs.rs:83` | 单次 read_file 上限 |
| `1000` / `50KB` | `pack-code/builtin/fs_ext.rs:15-17` | find 条数 / 输出上限 |
| `100` / `500` / `50KB` | `pack-code/builtin/search.rs:20-26` | grep 匹配数 / 行宽 / 输出上限 |
| `120s` | `pack-code/builtin/bash.rs:93` | bash 默认超时 |
| `10` / `5MiB` | `loop/core/images.rs:15-17` | 单消息图片数 / 单图字节 |
| `8` | `daemon/service.rs:243-247` | 并发任务上限（`THUNDER_MAX_CONCURRENT_TASKS`） |
| `120_000` | `daemon/service.rs:24` | 请求超时（双处一致） |
| `1024` / `64KB` | `daemon/service.rs:250,260` | stdout 通道深度 / 批写阈值 |
| `256` / `16` | `daemon/service.rs:105,330` | 会话模式上限 / MCP 池上限 |
| `1800s` / `60s` | `daemon/ask_user.rs:24` / loop `types/ui.rs:28` | 提问超时 / 弹窗超时 |
| `120s` | root `plugins/skills.rs:88,95` | 技能目录 TTL |
| `5` / `256KB` | root `plugins/memory.rs` | 导入深度 / 渲染预算 |
| `100` | root `plugins/skills.rs:127` | 技能描述截断字符数 |
| `30` / `40` | conversation `types.rs:640,224` | provisional 标题 / 自动标题字符数 |
| `4` | tui `app.rs:3471` | `/compact` 保留消息数（硬编码） |

### 18.3 文档漂移

> 2026-10-13 做过一轮全仓文档同步，下列前三条已修；余下几条仍是现存漂移。

- ~~`thunder-agent-root/README.md` 仍写 `with_standard_plugins()` 与 `ScriptPlugin`~~ **已修**（0.4 就删掉了 TS 插件宿主，README 已改写）。
- ~~`KEYBOARD.md` 记录 `/pipeline`、`/parallel`、`/fanout`~~ **已修**（三者既不在 `ALL_COMMANDS` 也没有执行分支，已从表里删净）。
- ~~`providers/README` 的方言表与代码不符~~ **已修**（表里补上了 `openai-responses`，与 `client.rs:86-104` 的三种方言一致）。
- `commands.rs:52` 的注释写"27-command table"，实际 31 条（代码注释，未动）。
- `KEYBOARD.md` 未记录 `Ctrl + B`（侧栏），而 `app.rs:1666` 与 `/help` 都实现了它。
- daemon 默认 `EnvFilter` 里的 target `thunder_daemon` 与本 crate 名 `thunder_agent_daemon` 不匹配（`main.rs:22`），该 debug 过滤实际不生效。`[INFERENCE]`

### 18.4 已实现但无生产调用方（死接线）

| 接口 | 定义处 | 状态 |
|---|---|---|
| ~~`ActivePluginSet::dispatch_event` → `ThunderPlugin::on_event`~~ | ~~`root/registry.rs:164`~~ | **已接通**：`RootRunHandle::take_events`（`root/host.rs:113`）代理事件流并逐条派发；`ConversationPlugin::on_event` 的逐 turn 增量持久化（`conversation.rs:241-256`）随之生效 |
| ~~`ThunderPlugin::tool_gate`~~ | ~~`root/plugin.rs:297`~~ | **随插件框架删除**：gate 现在由宿主在构造 `ToolExecutor` 时显式安装（`host.rs` 的 `.with_tool_gate(...)`），没有"声明了但没人读"的路径 |
| `ToolInvoker::invoke` / `ExecutorToolInvoker` | `loop/types/invoke.rs:53,120` | 插件反向 RPC 全链路可用且有 trace 测试，但无随仓插件使用；槽位由 `host.rs:737-745` 填充 |
| `install_code_pack` / `CodePackConfig::apply_default_prompt` | `pack-code/pack.rs` | 仅测试调用；字段从未被读取（仍存在） |
| ~~`render_sidebar`~~ | ~~`tui/ui/sidebar.rs:9`~~ | **已修**：`ui/mod.rs:66` 在 `show_sidebar` 时调用 |
以上四项是"接线存在、无人拉"；`thunder-runtime` 是独立二进制、`MemoryConversationStore` 由 TUI 的 `StandardHostBuilder` 正常使用，**不属于**死接线。（`McpServerConfig::timeout_ms` 曾在此列，已在 §15.2.2 接上消费者，移出。）

### 18.5 已知功能缺陷

| 缺陷 | 严重度 | 证据 | 后果 |
|---|---|---|---|
| ~~**stdio MCP server 在 spawn 返回时即被 SIGKILL**~~ | ~~阻断~~ **已修复** | 根因与修复见 §15.2.1；回归测试 `mcp/tests/stdio_lifecycle_test.rs` | 修复前 stdio MCP 完全不可用且静默（`McpPlugin::on_init` fail-open + 测试只用 `MockTransport`） |
| ~~`tools/call` 无超时 + pending 表泄漏~~ | ~~高~~ **已修复** | 根因与修复见 §15.2.2；回归测试 2 条 | 修复前一个不响应的 server 会永久挂住工具调用，且每次超时留一条 pending 残留 |
| 忽略 `nextCursor` | 中 | `protocol.rs:144` 解析、无人读取 | 多页工具列表被截断 |
| MCP 图片内容被丢弃 | 中 | `protocol.rs:184-188` 只渲染 `[Image: mime]` | 视觉结果到不了模型 |
| ~~`read_file` 的 10 MB 上限可被任意 `limit` 绕过~~ | ~~中~~ **已修复** | 大文件不再走整读：`limit`/`offset` 只选中流式窗口（`BufReader::lines`，`pack-code/builtin/fs.rs` 的 `read_streamed`），流式输出另有 512 KiB 预算封顶并带截断提示；无分页的大文件仍拒（原文案不变）；stat-then-read 竞态有事后长度复核。回归测试 `pack-code/tests/code_tools_test.rs` 的 `read_file_large_*` 两条 | 传 `limit` 即绕过、大文件整读入内存的问题已消除 |
| ~~`ActivePluginSet::dispatch_event` 无调用方~~ | ~~中~~ **已修复** | 根因与修复见 §11.1.1；`RootRunHandle::take_events` 现为唯一派发点 |
| 技能目录会把 `README.md` 当技能加载 | 低 | `skills/loader.rs:81-86,70-74` | 系统提示里出现意外技能条目 |
| ~~`ContextInput::Messages`/`Buffer` 让转录里的旧 system 消息压过宿主新装配的提示~~ | ~~高~~ **已修复** | `engine.rs` 的 `spawn_loop` 现在对所有输入形态统一 `set_system_prompt`（`config.system_prompt` 是该 run 的事实来源）；回归测试 `loop/tests/system_prompt_authority_test.rs` 4 条 | 修复前 AGENTS.md / `systemPromptFile` / 技能目录 / 插件提示在 TUI 与 daemon 上从第二轮起全部失效 |
| ~~运行中切换会话会把两次 run 的输出拼进同一份转录~~ | ~~高~~ **已修复** | `new_session`/`load_conversation` 在 `cancel_token` 或非 Idle 状态时拒绝切换（`app.rs` 的 `refuse_session_switch_while_running`）；`AgentFinished` 现在携带 `run_id`，与 `active_run_id` 不符的迟到结果被丢弃；回归测试 `tui/tests/run_lifecycle_guard_test.rs` | 修复前 Ctrl+N / `/new` / 侧栏 LoadSession 在 run 中会污染目标会话 |
| ~~`MaxTurnsExceeded`/`BudgetExceeded` 时丢弃引擎转录~~ | ~~中~~ **已修复** | 非 `Done` 分支先采用 `authoritative_messages`，预算耗尽视为可续跑而非错误 | 修复前下一轮从宿主不完整的视图继续，工具结果丢失 |
| ~~`/config max_turns` 只改显示值~~ | ~~中~~ **已修复** | TUI 两条执行路径改用 `with_max_turns(self.max_turns)`；`0` 被拒绝 | 修复前该配置项对实际运行无影响 |
| ~~daemon 把 `Cancelled` 报成 `TaskCompleted`~~ | ~~中~~ **已修复** | `service.rs` 现在把 `Cancelled` 与 `Error` 一并映射到 `TaskFailed` | 修复前客户端无法区分"跑完了"与"被取消" |
| ~~runtime 的 pending 工具表只按 `call_id` 索引、且等待不可取消~~ | ~~中~~ **已修复** | 键改为 `(run_id, call_id)`；`RemoteTool`/`RemoteGate` 用 `select!` 监听 run 的取消令牌；回归测试 `runtime/src/remote.rs` 3 条 | 修复前跨 run 的迟到 `tool_result` 可能答错调用，取消后任务仍挂起 |
| ~~`ThunderRoot::run()` 不消费事件流~~ | ~~中~~ **已修复** | 该便捷路径现在 `take_events()` 并排空，插件 `on_event` 与状态/指标代理随之生效 | 修复前 `run()` 的插件全程静默 |
| 结构化事件送达超过 5s 即被计数并告警（现另有 `max_event_bytes` 字节预算，见 §6） | 中 | `engine.rs` 的 `EventDropCounters`（`types/event.rs`）：micro-delta 走 `try_send` 计数，结构化事件 5s  deadline 未送达计数 + `warn!`，`AgentLoop::event_drop_counters()` 可读，结算时汇总告警。仍不阻塞引擎（行为不变），但不再是"静默丢弃" | 消费者被压满超过 5 秒时宿主可从计数得知记录不完整（曾写作"不丢"，现为"丢了能知道"） |
| ~~审批弹窗不带"到底要跑什么"~~ | ~~中~~ **已修复** | 门算出 `detail = describe_call(args)`，内核经 `UiRequest::Select.detail` 透传（`executor.rs` 的 `ask`），TUI 拼进问题正文、daemon 经 flatten 透传给面板（`protocol.rs` 的 `UiRequest` 变体），缺省旧面板忽略。回归测试 `loop/tests/approval_detail_test.rs` + TUI `tui_approval_dialog_shows_what_is_being_approved` | 用户看到的不再只是 `Run bash` |
| ~~TUI 的 `/save`、`/rail`、`/status` 别名宣称但未实现~~ | ~~低~~ **已修复** | 分派分支现在同时认别名（`app.rs` 的 `"export" \| "dump" \| "save"`、`"timeline" \| "rail"`、`"health" \| "doctor" \| "status"`） | 别名不再落到"未知命令" |
| ~~run 结束后仍持有运行的 trace sink（事件通道永不关闭）~~ | ~~阻断~~ **已修复** | `StaticToolsFactory::for_run` 把 sink 装进共享 executor 槽位却不清除；修复为 generation-guarded `install_run_trace` / `clear_run_trace` + `RegistryTools::Drop`（`policy/registry_tools.rs`）。回归测试 `loop/tests/run_lifecycle_test.rs` | 修复前所有"等事件排空再 join"的宿主永久挂起，CI 的 workspace suite 跑不完 |
| ~~plan 模式下提供 "always allow"，但 plan 永不查规则~~ | ~~中~~ **已修复** | `ask_for` 在 `Mode::Plan` 下把 `allow_always` 置为 `false`；内核仍拒绝"没提供却答了"的 `AlwaysAllow`（`executor.rs`）。回归测试 `plan_mode_test.rs` 的两条成对用例 | 修复前用户点"总是允许"后下一次同样调用照旧弹窗 |
| ~~MCP 单帧响应无上限~~ | ~~高~~ **已修复** | `max_frame_bytes`（默认 4 MiB）在读取过程中截断；超限帧丢弃并让在途请求立即失败（`stdio.rs` 的 `read_frame`）。回归测试 `stdio_lifecycle_test.rs::an_oversized_reply_is_discarded_and_fails_the_call` | 修复前一个 100 MB 响应会整段进堆 |

---

## 19. 验证与测试

### 19.1 门禁

```bash
python3 scripts/check-version.py        # 版本一致性（发布时加 --tag vX.Y.Z）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features thunder-agent-daemon/testing-mock -- -D warnings
cargo check --workspace --all-targets --features thunder-agent-daemon/testing-mock
./test.sh          # cargo test --workspace --no-fail-fast --quiet --features .../testing-mock
```

（`.github/workflows/ci.yml`、`test.sh`）`RUSTFLAGS="-D warnings"` 在 CI 里全局生效。

**版本只有一处真源**：根 `Cargo.toml` 的 `[workspace.package].version`，10 个 member 一律 `version.workspace = true`。`scripts/check-version.py` 是这条规则的唯一实现，ci.yml、release.yml、test.sh 三处调用（避免两份检查各自漂移）：它要求 member 继承版本、`Cargo.lock` 跟上、`docs/architecture.md` 记录的版本一致，并在发版时要求 tag 等于 `v<版本>` 且不低于任何已有 tag —— **只改了某个子包也必须提升版本号**，否则消费方只看版本号分不清是哪个构建。release.yml 的 checkout 因此用 `fetch-depth: 0`（浅克隆读不到 tag，门禁会拒绝而不是放行）。


### 19.2 测试分布

| Crate | 集成测试文件 | 覆盖重点 |
|---|---|---|
| loop | 22 个（`tests/*.rs`） | 单元契约（并行/忙/取消隔离）、完整生命周期、流中断三分支、checkpoint 压缩与紧急裁剪、溢出自动修复、pause/steer、trace 的 14 条出口、UTF-8 安全、缓存保温；新增 `event_budget_test`、`compaction_concurrency_test`、`run_lifecycle_test` |
| pack-code | 10 个 | 工具行为、守卫、路径作用域、权限层、插件调用、安全守卫；新增 `end_to_end_test`（真实 read → 改 → 跑检查） |
| root | 19 个 | 模式矩阵 15 格、plan 模式端到端、host UI、记忆写入 jail、配置分层、无 gate 全放行、执行状态流；新增 `mcp_pack_parity_test`（pack 与 MCP 工具行为一致） |
| daemon | 3 个 | 真实二进制的 IPC（`CARGO_BIN_EXE_thunder-daemon`）、模式协议、数据根隔离 |
| conversation | 6 个 | CRUD/持久化门禁/多拓扑/构造竞态/标题 |
| tui | 22 个 + 模块内单测 | 运行流与控制、命令与补全、chrome、输入框、metrics、滚动、transcript 缓存与顺序、时间轴、markdown、链接、会话生命周期、picker、窗格状态（OSC 7501） |
| providers | 2 个 | 目录解析、思考档位、compat/cost 透传、保温推导、元数据缓存 |
| skills / mcp / runtime | 1 / 3 / 0 个 | 解析/发现/桥接、JSON-RPC 往返；**`runtime` 没有 `tests/` 集成测试文件**（其 JSONL 协议只由 crate 内单测覆盖）；`mcp/tests/stdio_lifecycle_test.rs` 用真实 `sh` 子进程验证 spawn 存活 + close 终止 + 静默 server 超时/不残留 + 超限帧丢弃 |

### 19.3 关键回归测试（行为契约）

| 测试 | 钉住的性质 |
|---|---|
| `loop/tests/trace_test.rs::every_exit_path_records_a_trace` | 遍历 14 条出口，"每次调用一条 trace"是结构性的 |
| `loop/tests/trace_test.rs::a_trace_cannot_be_disabled` | 记录不可关闭 |
| `loop/tests/unit_contract_test.rs::second_start_on_busy_unit_is_already_running` | 单任务闸门 |
| `loop/tests/cancel_continuity_test.rs::cancelling_at_a_tool_boundary_answers_the_tool_calls` | 取消后不留悬空 tool_call |
| `loop/tests/steer_test.rs::steering_never_interrupts_an_in_flight_tool` | 转向只在轮次边界 |
| `loop/tests/pause_gate_test.rs::cancel_wins_over_a_pause` | 取消优先于暂停 |
| `loop/tests/checkpoint_compaction_test.rs::failed_summarization_falls_back_to_emergency_trim` | 摘要失败仍有安全网 |
| `root/tests/mode_matrix_test.rs::a_rule_does_not_leak_into_plan_mode` | plan 跳过 allow 规则 |
| `root/tests/plan_mode_test.rs::a_hostile_session_id_cannot_escape_the_plans_directory` | session id 消毒是唯一防线 |
| `root/tests/gate_absent_allows_test.rs::without_a_gate_everything_the_guard_allows_runs` | 无 gate 全放行是契约 |
| `root/tests/agent_status_stream_test.rs::a_run_publishes_its_status_and_reports_a_host_reported_wait_without_an_event` | 状态帧只在跃迁时出现、工具阶段带工具名，且**宿主补报的等待在运行的模型请求仍在途中（一条内核事件都没有）时也能送达消费者**——`take_events` 那个 `select!` 存在的唯一理由 |
| `pack-code/tests/path_scope_test.rs::a_dangerous_command_is_refused_in_every_mode` | 守卫在 gate 之前 |
| `daemon/tests/thunder_home_isolation_test.rs::daemon_writes_only_under_the_injected_root` | 数据根可被整体重定向 |
| `conversation/tests/durability_test.rs::fs_store_refuses_a_conversation_without_a_user_turn` | 持久化门禁（连目录都不建） |
| `conversation/tests/store_concurrency_test.rs::concurrent_new_on_a_clean_root_succeeds` | 构造竞态重试 |
| `tui/tests/transcript_order_test.rs::thinking_survives_the_authoritative_replacement` | thinking 重新附着 |
| `tui/src/links.rs::dangerous_schemes_are_rejected`（模块内单测） | `javascript:`/`data:` 等被丢弃 |
| `tui/tests/pane_status_test.rs::a_run_walks_the_pane_from_working_through_its_tool_to_done` | 窗格报告的字面形状：`app=thunder`、绝不带 `id=`、阶段词表与 `kind=` 的取舍——**报错的格式等于没报**，所以断言直接打在字节上 |
| `mcp/tests/stdio_lifecycle_test.rs::a_real_stdio_server_survives_spawn_and_serves_requests` | 真实 stdio server 在 spawn 后存活，且 `close()` 真的终止进程（`pgrep` 归零） |
| `mcp/tests/stdio_lifecycle_test.rs::a_silent_server_times_out_and_leaves_no_pending_entry` | 静默 server 不挂住调用（可配置时限），且超时不留 pending 残留 |
| `mcp/tests/stdio_lifecycle_test.rs::an_explicit_zero_timeout_opts_out_of_the_bound` | `timeout_ms = 0` 真的放弃时限；`close()` 释放所有 parked 请求 |

### 19.4 本地验证记录

本文档主体基于静态阅读 + `path:line` 交叉核对，未逐条执行为文档结论而设的测试。

**已实际执行的验证**：

```bash
# ① spawn 即被 SIGKILL（§15.2.1）：A/B
git stash push -- thunder-agent-mcp/src/transport/stdio.rs   # 回到缺陷源码
cargo test -p thunder-agent-mcp --test stdio_lifecycle_test   # → 2 failed（10s 握手超时）
git stash pop                                                 # 恢复修复
cargo test -p thunder-agent-mcp --test stdio_lifecycle_test   # → 2 passed

# ② 超时 + pending 泄漏（§15.2.2）：新增的两条测试
cargo test -p thunder-agent-mcp --test stdio_lifecycle_test   # → 4 passed（0.41s）

# ③ 门禁
cargo fmt --all -- --check                                     # 通过
cargo clippy -p thunder-agent-mcp --all-targets -- -D warnings  # 通过（exit 0）
./test.sh                                                      # 全工作区通过
```

**关于"全工作区通过"的时效性**：本次运行期间，工作树里同时存在**另一批未提交的在制品**（与本轮改动无关的 crate）；它一度让另一个 crate 的 lib 测试失败，等那批文件补完后重跑即全绿。也就是说 `./test.sh` 的结果取决于那一刻整棵树的状态，而 §15.2 的改动本身只落在 `thunder-agent-mcp`。

§18.5 中其余条目仍是**静态阅读得出、尚未修复**，未做运行时复现。

---

## 20. 快速索引：改一处代码该看哪里

| 想做 | 该动 |
|---|---|
| 加一个工具 | 实现 `AgentTool`（`loop/types/tool.rs:196-215`）→ 宿主 `register_tool`；若要特殊效果声明 `effect()` |
| 加一个审批策略 | 实现 `ToolGate`（`loop/types/gate.rs:118-142`），在宿主 `with_tool_gate` 安装 |
| 让某路径免问 | 实现/扩展 `ToolScope`（`loop/types/scope.rs:57-73`），注意它只能放宽 |
| 收 trace 到自定义存储 | 实现 `ToolTraceSink`（`loop/tools/trace.rs`）或排 `AgentEvent::ToolTrace` |
| 加一个提示词片段 | 实现 `PromptSection`（`root/assembly.rs`）→ `ThunderRoot::with_section` |
| 加一组工具 | `ToolPack::new(id).with_tool(...)` → `ThunderRoot::with_pack` |
| 加一个事件观察者 | 实现 `Observer`（异步、带 `RunEnv`）→ `ThunderRoot::with_observer` |
| 加一个按运行准备 | 实现 `Prepared` → `ThunderRoot::with_prepared` |
| 加一个 Phase / 状态词汇 | `root/plugins/status.rs` 的 `AgentPhase` + `apply_event`（**同时**改 TUI 的 `pane_status.rs::state_for` 与桌面面板的映射表，否则新档位会被协议丢掉） |
| 加一个斜杠命令 | `tui/commands.rs:69` 的表 + `tui/app.rs:2965` 的 match（两处都要改） |
| 加一个 UI 模块 | `tui/ui/mod.rs` 的 `draw` 里插一段布局 |
| 换模型方言 | `providers/convert.rs::SUPPORTED_APIS` + `client.rs::with_model` 的 match |
| 换会话存储后端 | 实现 `ConversationStore`（`conversation/store/mod.rs:11-37`） |
| 加一个 MCP 配置位置 | `mcp/config.rs:127-161` 的候选列表 |
