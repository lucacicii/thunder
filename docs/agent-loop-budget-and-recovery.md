# Thunder Agent Loop：预算、停滞检测与自愈改造方案 v1.0

> 状态：已定稿（4 项待拍板已决策，见 §16）
> 涉及 crate：`thunder-protocol` / `thunder-registry` / `thunder-executor` / `thunder-std` / `thunder-core` / `thunder-daemon` / `thunder-tui`

---

## 0. 摘要

当前 Agent Loop 的唯一全局终止条件是 `max_turns = 16`，且撞限时直接返回 `Err`、不做收尾、丢弃产出。同时 `LoopGuard` 的两条中止路径会留下不配对的 `tool_call`，污染持久化会话，使该会话此后永久 400。

本方案把「一个数字管一切」拆成三层互相独立、可分别验收的能力：

| 层 | 职责 | 产出 |
|---|---|---|
| **停滞检测** | 发现「没有进展」的循环（连击 / 周期 / 零新颖 / 错误率） | `StallSignal` |
| **全局预算** | 兜住「确实在进展」的合法长任务 | `FinishReason::{MaxTurnsExceeded, BudgetExceeded}` |
| **自愈阶梯** | 停滞发生后自动恢复，无需人工介入 | L0 策略推动 → L1 上下文重置 → L2 全新 attempt |

**目标**：长任务不再被 16 轮腰斩；卡死能自愈；任何一种退出都给出可读总结而非一行报错；上下文永不出现 dangling `tool_call`。

**非目标**：让 core 理解工具语义（微内核边界不动）；实现任意副作用的回滚（做不到，改为「带着已知状态继续」）。

---

## 1. 现状问题清单

| # | 问题 | 位置 | 影响 |
|---|---|---|---|
| P1 | 唯一全局上限是 16 轮硬 `Err`，不收尾、无 `final_response` | `agent_loop.rs:199` / `:556-561` | 正常长任务被腰斩，产出丢失 |
| P2 | `FinishReason::{MaxTurnsExceeded, BudgetExceeded}` 只有定义、零生产者 | `event.rs:24-30` | 协议意图未实现，下游无法区分「预算用尽」与「崩溃」 |
| P3 | LoopGuard 硬重复触发时 0 个 tool 结果即 `return Err` | `agent_loop.rs:441` | context 留下 N 个孤儿 `tool_call` |
| P4 | 熔断触发时同批剩余 call 被丢弃 | `agent_loop.rs:527` | 部分孤儿 `tool_call` |
| P5 | 批内 cancel 直接返回 | `agent_loop.rs:428-430` | 部分孤儿 `tool_call` |
| P6 | 无任何配对修复 | `providers/client.rs:270-315`（全仓库 grep `orphan/repair/sanitize` 无命中） | 孤儿 call 原样发 provider → 400；daemon `service.rs:922` 存盘后会话永久坏死 |
| P7 | LoopGuard 只判「连续 N 条完全相等」，对 A-B-A-B 周期全盲 | `loop_guard.rs` | 模型最常见的卡死形态漏检 |
| P8 | 连续错误计数被单次成功清零 | `loop_guard.rs:98-104` | 抖动式错误（err,err,ok）永不触发 |
| P9 | core 用工具名猜测哪个是写工具 | `checkpoint.rs:249-258` | 违反微内核边界，且对新插件失效 |
| P10 | 停滞/预算终止无自愈，只能人工重新提问 | 全局 | 交互体验差，长任务需人肉看护 |

---

## 2. 设计原则

1. **语义归插件，策略归 core。** 插件只声明「这个工具会不会改变世界」一个标签；阈值、窗口、退出动作全在 core。core 永不解析文件名、命令或输出格式。
2. **唯一指纹只在一处计算。** `progress::fingerprint()` 是全仓库唯一的进展哈希函数。插件不回传 hash，`ToolProvider::call_tool -> Result<String, _>`（`schema.rs:312-317`）的 ABI 不动。
3. **早退不得留下 dangling `tool_call`。** 新增硬不变式：任何 `return` 前，assistant 消息里每个 `tool_call` 必须有配对 `tool` 消息。
4. **两类预算不可互相替代。** 停滞判据能早停「假进展」；硬预算只能兜住「真进展的合法长任务」。后者删不掉，是数学不是工程。
5. **自动恢复不等于静默。** 每一次恢复都必须可观测、可中止，并占用同一份预算。

---

## 3. 协议层变更（`thunder-protocol`）

### 3.1 新增 `ProgressKind`（`src/schema.rs`）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    /// 契约：相同 args ⇒ 相同世界状态（幂等）。
    /// core 用 hash(args) 作为状态快照。
    Mutating,
    /// 只读。相同 args + 相同输出 = 无新信息。
    Observing,
    /// 副作用无法由 args 刻画，或来源不可信（bash、第三方 MCP）。
    /// 仅参与输出指纹兜底。
    #[default]
    Neutral,
}
```

> **契约红线**：`bash` 必须是 `Neutral`。它违反 Mutating 的幂等契约（`echo x > f` 的副作用不在 args 里）。这条必须写进 doc comment，否则后人给 bash 打上 `Mutating`，整个检测体系失效。

### 3.2 `ToolDescriptor` 加字段（`src/schema.rs:102-111`）

```rust
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub uri: String,
    #[serde(default)]
    pub progress: ProgressKind, // 默认 Neutral，向后兼容
}
```

`new()` 保持默认 Neutral，新增 `with_progress(ProgressKind)`。

**已核对安全性**：
- `message.rs:311-318` 的 `ToolDefinition::from(&ToolDescriptor)` 只取 `name/description/input_schema`，新字段**不会**进模型可见的 tools schema，KV 前缀缓存不受影响。
- `CatalogSnapshot.version`（`registry.rs:183`）是注册计数而非内容哈希，加字段不会误触发 `gateway.rs:64` 的 hot-reload 通知。

### 3.3 事件与统计

**不新增 `AgentEvent` 变体**，改用协议里已有的逃生舱 `AgentEvent::Custom { kind, payload }`（`event.rs:104-107`）：

```rust
// 预算/停滞信号（纯展示，不进类型枚举）
Custom { kind: "budget_notice".into(),  payload: { turn, reason, detail, used_turns, used_tokens, elapsed_ms } }
Custom { kind: "loop_recovered".into(), payload: { attempt, level, reason, handoff_tokens } }

// AgentStats 新增（全部 #[serde(default)]）
pub stall_events: usize,
pub mutations: usize,
pub novel_observations: usize,
pub finish_reason: Option<FinishReason>,   // 权威真实原因
```

**已核对桌面端（`~/wz/GitHub/agent-resume-panel`），三处容错足以支撑本方案，无需同步发版**：

1. `useThunderChat.ts:219-232` 的 `failureNoticeForFinishReason()` **已经实现了 `max_turns_exceeded` / `budget_exceeded` 的文案**——这两条是今天 thunder 从不产生的死代码，本方案正好把它们点亮。
2. `useThunderChat.ts:2166-2205` 与 `useTraceCollector.ts:255` 的 `case "custom"` 已按 `kind` 分派（`file_change` / `metrics_update`），未知 `kind` 直接 `break`，新 kind 对旧面板完全静默无害。
3. `packages/core/src/thunder/types.ts:227` 的 `finish_reason?: string` 是**裸 string 而非字面量联合**，新增取值不构成 breaking。

### 3.4 `finish_reason` 的单一规则（因上一条而必须明确）

桌面端 `loop_complete` 的既有契约是「**非 `done` 即表示本次 run 没有产出答案**」（`useThunderChat.ts:214-218` 注释原文）。收尾轮引入后，这一条会被打破：预算耗尽但收尾轮成功时，既有产出又有非 `done` 原因，面板会打出「...without producing an answer」与正文自相矛盾。

因此全方案统一为**一条规则**：

```
finish_reason = Done            iff final_response 非空
finish_reason = 真实原因        iff 完全无产出（收尾轮自身失败/被取消）
真实原因永远同时写入 AgentStats.finish_reason 与 budget_notice.payload.reason
```

`AgentLoopOutcome` 新增 `finish_reason: FinishReason`（真实原因）与 `attempts: usize`；写线上 `LoopComplete.finish_reason` 时套用上面的规则。daemon 的 `TaskEventBridge`（`service.rs:40-56`）用 `ObservedEvent` 无条件透传。

---

## 4. 插件声明（只改声明，不改调用）

| 文件 | 改动 |
|---|---|
| `thunder-std/src/tools.rs` | `write_file`(:127) / `edit_block`(:149) → `Mutating`；`read_file`(:101) / `grep`(:200) / `find`(:242) / `ls`(:267) → `Observing`；`bash`(:174) → `Neutral` |
| `thunder-registry/src/frontmatter.rs:13-19` | `RawScriptFrontmatter` 加 `#[serde(default)] progress: ProgressKind`，脚本工具可在 `.thunder/tools/*` 写 `progress: mutating` |
| `thunder-executor/src/mcp_client.rs:486` | 不动（默认 Neutral）。v2 可选：config 里给 server 声明 mutating 白名单 |
| `thunder-plan` / `thunder-ask` | 不动（默认 Neutral） |

**关键点**：`write_file` / `edit_block` 的 args 里本来就含完整文件内容，所以 `hash(args)` ≡ `hash(path + content)`。core 不需要知道「文件」这个概念，却精确得到「这一轮有没有新的写」。

**顺带清理**：`checkpoint.rs:249-258` 的 `is_mutating_tool_name()`（靠名字含 write/edit/create 猜测）改为读 `ProgressKind`，消除 P9。

---

## 5. core：`progress.rs`（新增）

### 5.1 唯一指纹函数

```rust
/// 全仓库唯一的进展指纹。digest 只在单次 run 的内存中存活，
/// 因此 DefaultHasher 足够（零新依赖），无需跨进程/跨版本稳定。
pub fn fingerprint(kind: ProgressKind, tool: &str, args: &str, output: &str) -> u64 {
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);  // 域分隔，避免跨类碰撞
    tool.hash(&mut h);
    match kind {
        Mutating  => args.hash(&mut h),                              // 幂等契约：args 即世界状态
        Observing => { args.hash(&mut h); output.hash(&mut h); }
        Neutral   => output.hash(&mut h),                            // args 不参与：不同命令同输出 = 真没进展
    }
    h.finish()
}
```

### 5.2 状态与判定

```rust
pub struct ProgressTracker {
    mutations: HashSet<u64>,
    observations: HashSet<u64>,
    turn_had_mutation: bool,
    turn_had_novel_output: bool,
    mutation_free_streak: usize,
    no_novel_output_streak: usize,
    pub total_mutations: usize,
    pub total_novel_observations: usize,
}

pub enum StallSignal { None, MutationFree(usize), NoNovelObservation(usize) }
```

每轮结束 `on_turn_end()`：

```
if 本轮有新 Mutating            → mutation_free_streak = 0
elif 整个 run 已出现过 Mutating → mutation_free_streak += 1
else                            → 恒为 0    // 纯研究型会话不判停滞

if 本轮有新输出指纹              → no_novel_output_streak = 0
else                            → no_novel_output_streak += 1

Stall(MutationFree)        if mutation_free_streak   >= 8
Stall(NoNovelObservation)  if no_novel_output_streak >= 6
```

第三条分支是关键：从头到尾只有 `read_file`/`grep` 的调研型任务，`total_mutations == 0`，绝不能被判停滞。停滞只在「已经开始动手、然后停手了」时成立。

指纹输入取自 `agent_loop.rs:494` 附近的 `normalized.to_llm_message()`（在拼接 `rep_warning` 之前）；args 复用 `:437` 已算好的 `args_serialized`。

---

## 6. core：LoopGuard 增强（`loop_guard.rs`）

保留现有三件套（软重复 3 / 硬重复 5 / 连续错误 5），新增三条。

### 6.1 周期检测算法

载体是**调用级指纹流**（不是轮级）：周期可能跨轮，也可能在一轮内部的多个 call 里，扁平序列自动归一两种形态。

```rust
const PERIODS: RangeInclusive<usize> = 2..=4;  // 故意排除 1
const WINDOW: usize = 24;

/// 尾部锚定的滞后-p 连续匹配。
fn detect_cycle(seq: &[(u64, bool /* 入场时是否全新指纹 */)]) -> Option<CycleHit> {
    let n = seq.len();
    let mut best: Option<CycleHit> = None;
    for p in PERIODS {
        if n < p + 1 { continue; }
        let mut pairs = 0usize;
        for i in 0..(n - p) {
            if seq[n - 1 - i] == seq[n - 1 - p - i] { pairs += 1 } else { break }  // 一断即停
        }
        if pairs >= 2 * p {                        // 至少 3 个完整周期才承认
            let cycles = 1 + pairs / p;            // 可见周期数
            if best.map_or(true, |b| cycles > b.cycles) {
                best = Some(CycleHit { period: p, cycles });
            }
        }
    }
    best
}
```

要点：

- `p = 1`（连续完全相同）是恒真判据，必须排除；那是现有连击规则的地盘，两套判据分工不可重叠。
- `pairs >= 2p` 才承认：`A-B-A-B`（4 元素）不判，`A-B-A-B-A-B`（6 元素）才判。
- 一断即归零，无需显式 reset。

### 6.2 停止阶梯与「零新颖」守卫

```rust
enum StallAction { None, SoftWarn(CycleHit), HardAbort(CycleHit) }

fn judge(seq: &[(u64, bool)], warned_until: usize) -> StallAction {
    let Some(hit) = detect_cycle(seq) else { return StallAction::None };
    let no_novel_in_window = !seq.iter().any(|(_, novel)| *novel);

    if hit.cycles >= 4 && no_novel_in_window { StallAction::HardAbort(hit) }
    else if hit.cycles >= 3 && seq.len() >= warned_until { StallAction::SoftWarn(hit) }
    else { StallAction::None }
}
```

| 级别 | 条件 | 动作 |
|---|---|---|
| 软告警 | `cycles >= 3`（`p=2` 时 6 个 call） | 不停止。notice 追加到当前 tool 结果尾部，`warned_until = len + 2p` 防抖 |
| 硬中止 | `cycles >= 4`（8 个 call）**且**窗口内零新指纹 | `close_pending_tool_calls()` → 进入自愈阶梯 |

**「零新颖」守卫是防误杀的关键**。周期性形状本身不足以定罪：`A B A B` 完全可能是正在推进的工作。附加「窗口 24 个调用无任何新指纹」后，误判率接近零——真正卡死的会话必然一个新东西都没有；只要模型多读一个新文件、多写一份新内容，守卫就不成立，最多软告警。

此处判「无新指纹」而非「无新 mutation」：`write a` / `write b` 反复写同样内容也属零新颖（`Mutating` 指纹 = `hash(args)`，内容没变即同一指纹），「来回搬砖」同样应被中止。

### 6.3 为什么指纹必须含 `hash(args)`

**必须中止的卡死**（`p=2`，第 6 个 call 软告警，第 8 个硬中止）：

| # | 动作 | 指纹 | 新颖 |
|---|---|---|---|
| 1 | `read_file(src/a.rs)` | fpA | ✅ |
| 2 | `grep("fn main")` | fpB | ✅ |
| 3 | `read_file(src/a.rs)` | fpA | ❌ |
| 4 | `grep("fn main")` | fpB | ❌ |
| 5 | `read_file(src/a.rs)` | fpA | ❌ |
| 6 | `grep("fn main")` | fpB | ❌ ← pairs=4 → cycles=3 软告警 |
| 7 | `read_file(src/a.rs)` | fpA | ❌ |
| 8 | `grep("fn main")` | fpB | ❌ ← pairs=6 → cycles=4 + 零新颖 → 硬中止 |

**绝不能中止的合法交替**（TDD 双文件推进）：

| # | 动作 | 指纹 |
|---|---|---|
| 1 | `write_file(a.rs, v1)` | fpA₁ |
| 2 | `write_file(b.rs, w1)` | fpB₁ |
| 3 | `write_file(a.rs, v2)` | fpA₂ |
| 4 | `write_file(b.rs, w2)` | fpB₂ |

内容一变指纹就变 → `pairs = 0`，永不命中。若用 `tool_name` 做指纹，这个正常场景会被直接腰斩——这是不接受「用工具名做周期判据」的原因。

### 6.4 其它增强

| 项 | 判据 | 动作 |
|---|---|---|
| 签名归一化 | 复用 `progress::fingerprint` 当签名，防 `ls .` / `ls ./` / 参数顺序逃脱 | — |
| 窗口错误率 | 最近 12 次里 error ≥ 8 | 硬断（保留连续 5 次作快通道） |
| 中止原因 | `abort_reason() -> Option<AbortKind>` | 供上层生成统一报告 |

---

## 7. 不变式：早退必须配平（修 P3/P4/P5/P6）

```rust
/// 为 calls[start..] 中每个未执行的 call 补一条 tool 消息，保证上下文配平。
fn close_pending_tool_calls(&mut self, calls: &[ToolCallRequest], start: usize, reason: &str);
```

必须在以下每个 `return` 之前调用：

| 位置 | 现状 | 后果 |
|---|---|---|
| `agent_loop.rs:441` 硬重复 | 0 条 tool 消息 | N 个孤儿 call |
| `agent_loop.rs:527` 熔断 | 只补当前 call | 部分孤儿 |
| `agent_loop.rs:428-430` 批内 cancel | 未补 | 部分孤儿 |

孤儿 `tool_call` 会被 `providers/client.rs:270-315` 原样发给 provider（无配对修复），下一轮 400；daemon `service.rs:922` 存盘后该会话**永久坏死**。此项独立于预算改造，应最先修。

### 7.1 Phase A 已落地

- `agent_loop.rs` 新增 `close_pending_tool_calls()`，接入三处早退（硬重复 / 熔断 / 批内 cancel）。
- 新增 `context::repair_tool_call_pairing()`：修复旧构建已落盘的损坏会话，在 `with_initial_messages()`（即所有入口的统一装载点）上做一次，补占位消息 + 剔除无主 tool 消息，并按请求顺序补齐。这比原先计划的「daemon 存盘前校验」更彻底：TUI / daemon / CLI 三个入口一次性覆盖，而且能治旧数据。
- `tests/loop_guard_context_balance.rs`：3 条回归（硬重复 / 熔断 / 批内 cancel）+ 1 条断言自检测试；已验证关掉修复后三条全红、孤儿 id 与预期一致。
- `context.rs` 单测 4 条：补齐、剔除孤儿、平衡 payload 不重写、占位符保序。

### 7.2 Phase B 已落地（观察层，不改控制流）

- `thunder-protocol`：`ProgressKind`（`Mutating`/`Observing`/`Neutral`）、`ToolDescriptor.progress`、`McpToolInfo.progress`（`skip_serializing_if` 让 `Neutral` 不出现在线上，第三方 server 报文保持逐字节不变）。
- 插件声明：`thunder-std` 7 个工具已标注（`write_file`/`edit_block` → Mutating，`read_file`/`grep`/`find`/`ls` → Observing，`bash` → Neutral）；脚本工具 frontmatter 支持 `progress:`，缺省 `neutral`。
- `thunder-core/src/progress.rs`：唯一指纹函数 `fingerprint()` + `ProgressTracker` + `StallSignal`。**novel mutation 也会写入结果集并重置「无新信息」计数**，否则纯写入型 run 会被误判停滞。
- `agent_loop.rs`：按冻结快照里的声明解析 kind、逐次 `record()`、轮末 `on_turn_end()`；命中只 `tracing::warn!` + `stats.stall_events += 1`，**不改变任何控制流**。
- `AgentStats` 新增 `mutations` / `novel_observations` / `stall_events` / `finish_reason`（均 `#[serde(default)]`）。

### 7.3 Phase D 已落地（强制生效）

- **收尾轮**：任何天花板（turns / tokens / wall clock / 停滞 / 周期）命中都走 `wrap_up_turn()`：注入 `<budget_notice>`（禁止调工具、最多 150 字总结）→ `complete_streaming(messages, &[], …)` → 返回 `Ok(outcome)`。收尾轮自身失败则回退到上下文里最后一段 assistant 文本；连那都没有才让线上 `finish_reason` 非 `done`。**不再有裸 `Err` 表示「跑到头了」。**
- **`finish_reason` 单一规则**：`AgentLoopOutcome::wire_finish_reason()` 实现 §3.4（有答案 → `done`）。daemon `TaskCompleted.finish_reason` 改用它；TUI 在 `finish_reason != Done` 时提示「可继续」；CLI 打印 summary 后退 0。
- **单一 turn 上界**：`AgentLoopConfig.max_turns` 删除，循环上界取自 `budget.max_turns`（`0` = 不限），KI-10 的双旋钮消失。默认值 16 → **200**。
- **turn 上限命中报 `max_turns_exceeded`，其余报 `budget_exceeded`**（面板对两者有不同文案）。
- **停滞早停**：`ProgressTracker::on_turn_end()` 命中 → `budget_notice{reason:"stalled"}` + 收尾轮。
- **周期检测**：`crates/thunder-core/src/cycles.rs`，签名 = `(kind, tool, args)`（**执行前**即可判定，能拦掉那次调用）；`pairs >= 2p` 才承认周期，软告警 3 周期，硬中止 4 周期 → 补平 tool 结果后收尾。
- **偏差记录**：§6.2 原定的「窗口零新颖」硬守卫被去掉。它实际上是空判据（周期的尾部按定义都是重复），真正的误杀保护来自签名含 args——内容演进的合法交替永远不构成同一签名。已在 `cycles.rs` 模块文档里写明。

### 7.4 Phase E 已落地（自愈 L0 + L1）

- **触发点**：停滞信号、周期硬中止。两者都先尝试 `try_recover()`，失败才走收尾轮。
- **L0 策略推动**：已经内建在软告警里（`loop_notice` / cycle notice 追加到 tool 结果尾部）。
- **L1 上下文重置** `context_reset()`：用现成的 `Summarizer` + `extract_file_lists()` 把卡死轨迹压成 handoff，重建为「system prompt + 原始目标 + `<handoff>`」。`fallback_handoff()` 在摘要器缺失/失败时降级为「已改文件 + 已读文件」清单——恢复失败绝不能让 run 崩溃。
- **硬约束**：`BudgetState::allow_context_reset()` 限制次数（默认 2），且**重置不充值** turns/tokens/wall-clock；重置后 `loop_guard` / `cycles` / `progress` 一并清零，`cache_warmer::invalidate()` 丢掉死前缀。
- **可观测**：`Custom{kind:"loop_recovered", payload:{turn, attempt, level, reason, detail, handoff_tokens}}`；TUI 渲染 `⟳ Auto-recovered (context_reset): …`。
- **L2（整轮重启）仍关闭**：`allow_restart()` 已实现但无人调用，`max_restarts` 默认 0。没有「换模型/换工具集/换计划模式」这类可变杠杆时，重跑只会复读同一个循环。
- 批内触发时用 `recovered_mid_batch` 标志跳出 tool 循环，`TurnEnd.finish_reason` 记为 `recovered`。

### 7.5 Phase C 已落地（预算会计，不强制）

- `thunder-core/src/budget.rs`：`LoopBudget`（静态上限）/ `BudgetState`（跨 attempt 累加）/ `BudgetUsage::{Ok, NearLimit, Exhausted}`。`allow_context_reset()` / `allow_restart()` 已就位但无人调用（Phase E）。
- 接线：轮首 `record_turn()` + `check()`；每次 LLM 响应后 `record_tokens()`。`NearLimit` 每个维度每次 run **只报一次**，以 `AgentEvent::Custom { kind: "budget_notice" }` 发出；`Exhausted` 目前只 `tracing::warn!`，**不中断**（Phase D 换成收尾轮）。
- 配置：`AgentSection` 新增 `wall_clock_secs` / `max_total_tokens` / `max_tool_calls_per_turn` / `warn_at` / `stall_*` / `max_context_resets` / `max_restarts`，`to_budget()` 投影。**`max_turns` 仍为 16**——预算上限必须和收尾轮同时上线，否则用户会拿到「大天花板但没有优雅出口」。
- TUI：新增 `AgentEvent::Custom` 渲染（`budget_notice` / `loop_recovered`，未知 `kind` 静默忽略），daemon 侧经 `ObservedEvent` 自动透传。
- **已知重复**：Phase C 里 `AgentLoopConfig.max_turns`（真正约束循环）与 `budget.max_turns`（仅观察）是两个旋钮，Phase D 合并为前者从 budget 取值。


---

## 8. 预算层

```rust
pub struct LoopBudget {
    pub max_turns: Option<usize>,        // 默认 Some(200)；None/0 = 不限
    pub wall_clock: Option<Duration>,    // 默认 Some(30min)
    pub max_total_tokens: Option<usize>, // 默认 Some(2_000_000)
    pub max_tool_calls_per_turn: usize,  // 默认 16
    pub warn_at: f32,                    // 默认 0.75
}

/// 跨 attempt 共享，绝不按 attempt 重置。
pub struct BudgetState {
    turns_used: usize,
    tokens_used: usize,
    wall_clock_start: Instant,
    restarts_used: usize,   // L2 次数
    resets_used: usize,     // L1 次数
}
```

检查点：轮首（turns / wall_clock / tokens）、单轮 tool 批开始（数量上限）、批内每 4 个 call（wall_clock 二次检查）。

超限的单个 call 必须**产出占位 tool 结果**（`[skipped: per-turn tool call limit 16 reached]`），不可静默丢弃——见第 7 节不变式。

---

## 9. 自愈阶梯

| 级 | 动作 | 上下文 | 成本 | 换掉的变量 |
|---|---|---|---|---|
| **L0 策略推动** | 注入 `<loop_notice>` 要求换策略 | 不动 | 0 | 提示 |
| **L1 上下文重置** | 摘要成 handoff，丢弃卡死轨迹，同 run 继续 | 重建 | 1 次摘要 + 1 轮 | 上下文 |
| **L2 全新 attempt** | 新 `ReActAgentLoop`，只带目标 + handoff 链 | 全新 | 重预热缓存 + 全量重做 | 必须额外换模型/思考深度/工具集/plan-mode |

**卡死的根因几乎总是「上下文被自己的失败轨迹污染」，不是「任务做不了」。** 因此真正能救回的是 L1；L2 在什么都不变的情况下重跑只会确定性地复读出同一个死循环。

### 9.1 L1 实现

现有基建已备好 80%：`Summarizer::summarize(region, prev_summary, read_files, modified_files, cancel)`（`checkpoint.rs:341-349`）、`extract_file_lists()`（`:261`）、`format_file_sections()`（`:293-311`）。

```rust
async fn context_reset(&mut self, hit: &CycleHit, calls: &[ToolCallRequest], idx: usize) -> Result<(), String> {
    // 1. 先配平本批剩余 call（第 7 节不变式）
    self.close_pending_tool_calls(calls, idx, "context reset");

    // 2. 复用现成摘要器
    let region = checkpoint::serialize_region(self.context.get_messages(), 0);
    let (read_files, modified_files) = checkpoint::extract_file_lists(self.context.get_messages());
    let handoff = match self.build_summarizer() {   // agent_loop.rs:563
        Some(s) => s.summarize(&region, None, &read_files, &modified_files, &self.cancel)
                       .await
                       .unwrap_or_else(|_| fallback_handoff(&read_files, &modified_files)),
        None => fallback_handoff(&read_files, &modified_files),
    };

    // 3. 重建上下文：system + 原始目标 + handoff
    self.context = ContextBuffer::with_system_prompt(&self.config.system_prompt);
    self.context.push(ChatMessage::user(self.goal.clone()));
    self.context.inject_system_notification(format!(   // context.rs:136
        "<handoff>\n你之前那次尝试陷入重复循环（周期 {p}，重复 {c} 次），已被中止。\n\
         以下是那次尝试留下的有效信息。请基于它换一条完全不同的路径，不要重犯：\n{handoff}\n\
         已修改的文件：\n{modified}\n</handoff>",
        p = hit.period, c = hit.cycles,
    ));

    // 4. 被丢弃的前缀无价值，别让它污染缓存预热
    cache_warmer::invalidate(&self.config.session_id);
    self.loop_guard.reset();
    self.cycles.reset();
    Ok(())
}
```

- `fallback_handoff()`：摘要调用可能失败（网络/取消），降级为「已改文件列表 + 最后 3 轮 tool 结果截断」。**恢复失败绝不能把 run 弄崩。**
- 摘要 prompt 必须显式声明「你上一次尝试失败的原因」。否则模型以为自己第一次上场，重走一遍——这是 L1 有效性的关键一句。

### 9.2 硬约束

1. **预算跨 attempt 共享**：`turns_used` / `tokens_used` / `wall_clock_start` 只累加不重置。否则自动重启就是「无限循环」的马甲，只是把循环从 turn 级搬到 attempt 级。
2. **cancel token / pause gate / steer queue 跨 attempt 复用**，`CancellationToken` 绝不重建。
3. **每一级必须改变至少一个变量**（见 9 节表）。L2 无杠杆时不执行，直接进收尾轮——诚实告败优于假装重试。
4. **副作用不可回滚**，所以 handoff 必须携带 `modified_files`。否则新尝试会假设「旧文本还在」（`edit_block` 依赖此前提）而连环报错。
5. **三类情形禁止自动恢复**：

| 情形 | 原因 |
|---|---|
| 用户 cancel | 人为意图，红线 |
| wall_clock / token 预算耗尽 | 恢复 = 绕过预算；要续跑只能由用户手动说「继续」（新授权） |
| 连续工具错误熔断（`is_circuit_broken`） | 多为环境故障（MCP 进程死亡 / 路径不存在 / 权限），换上下文无用 |

**唯一该自动恢复的是「停滞」**（周期循环、零新颖、软告警后仍不换策略）。

### 9.3 收尾轮（WrapUp）

任何超限/停滞的最终出口，不再裸 `Err`：

1. `context.inject_system_notification(...)` 注入：
   `<budget_notice>预算已用尽（{reason}）。禁止再调用任何工具。用不超过 150 字总结：已完成 / 未完成 / 建议的下一步。</budget_notice>`
2. `llm.complete_streaming(messages, &[] /* tools 置空 */, turn, sink, cancel)`
3. push assistant 消息 → 发 `Custom{kind:"budget_notice"}`（携带真实原因）→ `TurnEnd` + `LoopComplete { finish_reason: Done }` → `Ok(outcome{ finish_reason: 真实原因 })`
4. **收尾轮自身失败也不抛 Err**：用最近一轮 assistant 文本合成兜底 `final_response`；若连兜底文本都为空，才把线上 `finish_reason` 置为真实原因（触发面板的失败卡片）

规则见 §3.4：线上 `finish_reason` 只在「完全无产出」时才非 `Done`。

### 9.4 交互契约

自动 ≠ 静默：

- 新增 `Custom{kind:"loop_recovered"}` / `Custom{kind:"budget_notice"}`，经 `ObservedEvent` 自动透传到桌面端（旧面板按未知 `kind` 静默忽略）。
- TUI 推可折叠 `SystemNotice`：「⚠ 检测到重复循环，已自动重置上下文并重试（重置 1/2）」；状态栏显示当前 attempt 编号。
- `cancel` 全程有效，且 cancel 后不再恢复。
- daemon 无人值守场景默认 `max_restarts = 0`（不重启 L2），仅允许 L0+L1；有人值守时 L1 上限 2。

---

## 10. 配置（`config.rs`）

```toml
[agent]
max_turns = 200                 # 硬 backstop；0 = 不限
wall_clock_secs = 1800          # 0 = 不限
max_total_tokens = 2000000      # 0 = 不限
max_tool_calls_per_turn = 16
warn_at = 0.75
stall_mutation_free_turns = 8
stall_no_novel_turns = 6
cycle_periods = [2, 3, 4]
cycle_window = 24
cycle_soft_cycles = 3
cycle_hard_cycles = 4
max_context_resets = 2          # L1
max_restarts = 0                # L2，daemon 侧配 1
```

`default_max_turns()`（`config.rs:67`）16 → 200。同步 `.thunder/config.toml:4` 与 `README.md:62`。e2e 测试（`tests/e2e_microkernel.rs:88`）保持显式 `max_turns: 8`。

---

## 11. 下游改动

| 位置 | 改动 |
|---|---|
| `thunder-daemon/src/service.rs:920-960` | `Ok(outcome)` 按 §3.4 规则映射字符串（有产出 → `done`；无产出 → `max_turns_exceeded` / `budget_exceeded`）；仅真正 `Err` 走 `TaskFailed{Error}` |
| `thunder-daemon/src/service.rs:857` | `AgentLoopConfig` 装配新预算字段 |
| `thunder-tui/src/runner.rs:218-241` | 预算/停滞结束不再进 `Run ended: {err}`；改为「已达 {reason}（{turns} 轮 / {duration}）— 输入"继续"可接着跑」 |
| `thunder-tui/src/app.rs:1058` | `LoopComplete` 分支按 `finish_reason` 展示，非 `Done` 时不显示「Completed」；新增 `AgentEvent::Custom{kind:"budget_notice"|"loop_recovered"}` 渲染（TUI 目前是 `_ => {}` 吞掉，需显式匹配） |
| `thunder-core/src/main.rs:125-132` | 撞预算打印 summary 后 `exit(0)`（而非 `exit(1)`） |
| `thunder-tui/src/runner.rs:191` / `thunder-core/src/main.rs:111` | `AgentLoopConfig` 新字段装配 |

上下文已持久化 + 前缀缓存有效，因此「继续」的续跑成本极低。

---

## 12. 文件级改动清单

| 文件 | 类型 | 内容 |
|---|---|---|
| `thunder-protocol/src/schema.rs` | 改 | `ProgressKind`、`ToolDescriptor.progress`、`with_progress()` |
| `thunder-protocol/src/event.rs` | 改 | 仅 `AgentStats` 四字段（事件走既有 `Custom`，不加变体） |
| `thunder-protocol/src/lib.rs` | 改 | 导出新类型 |
| `thunder-core/src/progress.rs` | **新增** | `fingerprint()`、`ProgressTracker`、`StallSignal` |
| `thunder-core/src/cycles.rs` | **新增** | `detect_cycle()`、`CycleDetector`、`StallAction` |
| `thunder-core/src/budget.rs` | **新增** | `LoopBudget`、`BudgetState`、检查点逻辑 |
| `thunder-core/src/agent_loop.rs` | 改 | 预算接线、进度/周期接线、`close_pending_tool_calls()`、`wrap_up_turn()`、`context_reset()`、`run_with_recovery()` 外层循环、`finish_reason` 贯穿 |
| `thunder-core/src/loop_guard.rs` | 改 | 指纹归一化、窗口错误率、`abort_reason()` |
| `thunder-core/src/pruning/checkpoint.rs` | 改 | `is_mutating_tool_name()` → 读 `ProgressKind` |
| `thunder-core/src/config.rs` | 改 | 新配置项 + `default_max_turns()` 200 |
| `thunder-core/src/lib.rs` | 改 | 导出 `progress` / `budget` / `cycles` |
| `thunder-std/src/tools.rs` | 改 | 7 个工具标注 `ProgressKind` |
| `thunder-registry/src/frontmatter.rs` | 改 | 脚本 frontmatter 支持 `progress:` |
| `thunder-daemon/src/service.rs` | 改 | `finish_reason` 映射、预算装配 |
| `thunder-tui/src/{runner,app}.rs` | 改 | 新事件渲染、续跑提示、非 `Done` 展示 |
| `thunder-core/src/main.rs` | 改 | 退出码、预算装配 |
| `.thunder/config.toml` / `README.md` | 改 | 配置文档同步 |
| `docs/agent-loop-budget-and-recovery.md` | 新增 | 本文档 |

---

## 13. 分阶段落地

| Phase | 内容 | 是否改行为 | 预估 |
|---|---|---|---|
| **A 修漏** ✅ 已落地 | 第 7 节早退不变式 + 装载期修复 + 孤儿 `tool_call` 回归测试 | 否（纯 bug 修复） | 半天 |
| **B 观察** ✅ 已落地 | §3.1/3.2/4/5 + `tracing` 打点（每次工具调用打印 kind/novel/streak） | 否，`max_turns` 仍 16 | 1 天 |
| **C 接线** ✅ 已落地 | 预算层（`budget.rs`）+ `warn_at` 只 warn 不收尾 + 配置旋钮 + TUI `Custom` 渲染 | 极小 | 半天 |
| **D 放开** ✅ 已落地 | 收尾轮 + `max_turns=200` + 停滞早停 + 周期检测启用 + `finish_reason` 贯穿 | 是 | 1 天 |
| **E 自愈** ✅ 已落地 | L0 + L1（`max_context_resets=2`），L2 留开关 | 是 | 1 天 |
| **F 可选** | `ToolOutcome{ text, progress: Option<ProgressSignal> }` + `From<String>`，让 bash / 第三方 MCP 自报 mutation digest | 否（默认退化） | 1 天 |

**Phase A 应最先独立合入**：纯 bug 修复、零争议、与预算方案解耦，而且它今天就在污染用户会话。

**Phase B 必须先跑**：停滞判据参数（8 / 6 / 16 / 24）是经验初值，只能靠真实任务流校准。

---

## 14. 测试计划

**单测**
- `fingerprint`：Mutating 同 args 不同输出 → 同指纹；Observing 同 args 不同输出 → 不同指纹；Neutral 不同 args 同输出 → 同指纹
- `ProgressTracker`：纯 `read_file` 会话永不判停滞；`write` 一次后连续 8 轮只读 → 第 8 轮触发
- `detect_cycle`：`A-B-A-B` 不命中；`A-B-A-B-A-B` → `(p=2, cycles=3)`；`A-B-C-A-B-C-A-B-C` → `(p=3, cycles=3)`；`A-B-A-B` 后插入新元素 → 立即归零
- `judge`：周期 4 但窗口有新指纹 → 仅软告警；周期 4 + 零新颖 → 硬中止
- `LoopGuard`：err/err/ok 抖动在窗口 12 内命中硬断
- **回归（关键）**：构造 3 个 `tool_call` 的 assistant 消息，在第 2 个触发熔断 → 断言 context 里 3 条 tool 消息齐全
- **回归**：批内 cancel → 断言同批所有 call 均有配对 tool 消息

**集成**（mock `LlmDriver` 脚本化）
- 20 轮不同 `bash`（Neutral）→ 不判停滞
- 连续 10 轮相同 `read_file` → 第 6 轮软告警、第 8 轮硬中止 → L1 重置 → 断言新上下文的 system/user 结构与 `<handoff>` 内容
- `wall_clock = 1s` + 慢工具 → `BudgetExceeded` 且返回 `Ok`
- L1 后仍卡死 → `resets_used` 达上限后进收尾轮，`finish_reason = BudgetExceeded`
- 用户 cancel → 立即返回，**不**触发任何恢复

---

## 15. 风险与取舍

1. **`Mutating` 契约靠插件自觉。** 违反「相同 args ⇒ 相同状态」会导致误判为持续进展 → 由 `wall_clock` 兜底。
2. **纯 bash 工作流检测最弱。** 输出带耗时/PID 时指纹永远新鲜 → 只剩 `max_turns=200` + `wall_clock` 兜底。这是为微内核纯洁性付的价，Phase F 才能真正解决。**注意 200 轮意味着这个 backstop 很贵，`wall_clock` 才是真闸门，不可关。**
3. **收尾轮多一次 LLM 调用**（成本约 1 轮），换取用户看到总结而非报错。
4. **L1 丢弃轨迹会让前缀缓存失效**，需重新预热一次；但被丢弃的那段本来就没有信息价值。
5. **`AgentEvent` 是 tagged enum**，新增变体对其它 serde 消费者（desktop panel）是 breaking，需同步发版。
6. **默认值全部来自经验**，未经真实流量校准，Phase B 的打点是为了校准它们而不是走形式。

---

## 16. 决策记录

| # | 决策 | 影响 |
|---|---|---|
| 1 | `max_turns` 默认 **200** | `config.rs:67`、`.thunder/config.toml`、`README.md:62` |
| 2 | `ProgressKind` 进 `ToolDescriptor` **接受** | 脚本 frontmatter 新增可选 `progress:` 键，`#[serde(default)]` 向后兼容 |
| 3 | desktop panel 容错**已验证充分**；但需把预算/恢复事件改走 `Custom`，并确立 §3.4 的 `finish_reason` 单一规则 | 零 panel 改动、零同步发版 |
| 4 | L2 默认关闭（`max_restarts = 0`），仅 daemon 侧显式配 1 | 自愈只做 L0 + L1 |

剩余开放项（可在实现中再定）：`wall_clock` 30min 与 `max_total_tokens` 2M 是否适配你的典型任务；周期阈值经 Phase B 打点校准后再定。
