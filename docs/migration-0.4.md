# 迁移到 thunder 0.4.0

0.4.0 是一次 **breaking release**。它不是加功能，而是把三样东西从内核里搬走、把一样东西拆成两半：

| 动作 | 对象 |
|---|---|
| **删除** | 洋葱中间件栈（`ToolMiddleware` 家族）、tier 档位（`Permission`）、路径 jail（`security.rs`）、TS 插件宿主（`thunder-agent-plugin`） |
| **搬出内核** | 审批策略：`Mode` / `SessionPolicy` / `AllowRule` / 审批文案 → `thunder-agent-root` 的 `ModePlugin` |
| **拆成两半** | jail 的"知识"（路径打到哪 → `ToolScope`）与"策略"（准不准 → `ToolGate`） |
| **新增** | 单一执行入口 `ToolExecutor::run_tool`、每次调用一条的 trace、四个模式的机制级实现 |

谁需要读这份文档：

- **宿主嵌入者**（直接依赖 thunder 的 crate）：内核公共 API 收缩，见 §3；
- **agent-resume-panel (ARP)**：逐字段读 trace 文件、按 crate 名/目录找二进制、直接读写 `~/.thunder`，见 §7；
- **prototype-edit-llm (PS)**：pin 二进制 + 读 `file_change` 事件与 `FinishReason` 字符串，见 §7；
- **所有用户**：软 jail 是一次**净放宽**，见 §5——请先读它。

---

## 1. 协议差异

### 1.1 daemon（NDJSON over stdio）

| 项 | 0.3 | 0.4 |
|---|---|---|
| `ping` 响应 | `{pong, version}` | 追加 **`protocol: 2`** |
| 模式 | 无 RPC（`GetPermissionState` / `SetPermissionMode` 只有定义，**零消费者**） | `GetPermissionState {id, session_id}` → `{session_id, mode, rules}`；`SetMode {id, session_id, mode}` |
| 跑一次带模式的任务 | 无 | `run_task` 接受可选 **`mode`** 字段（`plan`/`ask`/`acceptEdits`/`yolo` 或旧名） |
| 插件重载 | `ReloadPlugins {id}` | **改名** `ReloadIntegrations {id}`（它清 MCP 池 + 失效 selection cache；TS 插件已删，旧名不再有意义） |
| 未知模式 | — | `SetMode` 返回 `success: false` + `unknown mode '…': expected one of plan \| ask \| acceptEdits \| yolo`，**绝不静默回落到默认模式** |

`version` 仍只回 `CARGO_PKG_VERSION`。**两者不是一回事**：`version` 每次发版都动，`protocol` 只在请求/响应形状变化时动——0.4 之前宿主无法区分"更新的 thunder"和"回答得不一样的 thunder"。

### 1.2 runtime（JSONL）

- `PROTOCOL_VERSION`：`"1"` → **`"2"`**（`thunder-agent-runtime/src/session.rs`）。它是 `ping`/`response` 帧里那个 `version` 字段的取值，语义就是"线上修订号"，与上面的 daemon `protocol: 2` 同步。

### 1.3 模式名

线上拼写（`mode` 字段的取值）：

```
plan | ask | acceptEdits | yolo
```

解析器（`Mode::parse`）**仍接受**历史名称，因为宿主没有别的办法知道它们被改过：

| 输入（大小写不敏感，`-` 与空格归一为 `_`） | 落到 |
|---|---|
| `plan` | `plan` |
| `ask` `default` `manual` `mutations` `writes` `always` `all` | `ask` |
| `acceptEdits` `accept_edits` `shell_only` `ask_shell` | `acceptEdits` |
| `yolo` `never` `none` `bypass` `bypass_permissions` `bypasspermissions` | `yolo` |

> ⚠️ **`plan` 的语义被修正了**：0.3 的 `PermissionMode::parse` 把 `"plan"` 解析成 `never`（免审放行），与"计划"完全相反。0.4 起 `plan` 就是 plan 模式。

---

## 2. 删除的东西

### 2.1 洋葱中间件栈

整个 `thunder-agent-loop/src/tools/middleware/mod.rs` 及其实现删除：

`ToolMiddleware`、`ToolHandler`、`ToolPipeline`、`MiddlewareRunner`、`RegistryTerminalHandler`、`PipelineContext`、`MiddlewareConfig`。

配套删除：

| 删除项 | 原因 |
|---|---|
| `MiddlewareConfig` 的 4 个 bool | 层开关随层一起消失 |
| 环境变量 `THUNDER_DISABLE_SECURITY`、`THUNDER_DISABLE_TX` | 同上 |
| `without_transactions()` / `without_middlewares()` | 同上 |
| `pack-code/src/middleware/output.rs` | 与 `ToolRegistry` 的 sanitize + head/tail 截断逐字重复 |
| `middleware/resource.rs` 的 10MB 上限 | 与 `builtin/fs.rs` 逐字重复 |
| `pack-code/src/middleware/security.rs`（1594 行） | 见 §4 |
| `pack-code/src/middleware/transaction.rs` | 有价值的部分**下沉**：cancellation 分支、cross-device(EXDEV) fallback、per-path `FILE_MUTATION_LOCKS` 搬进 `builtin/fs.rs`；`write_plan` 共用同一把锁 |
| `pack-code/src/middleware/file_change.rs` | `file_change` 事件的**发出下移**到工具层（形状不变），bash 的 `git status --porcelain` 差分搬进 `BashTool::execute` |

### 2.2 tier 档位

| 删除项 | 位置 |
|---|---|
| `Permission`（`Read`/`Write`/`Bash`） | 内核 |
| `AgentConfig.permission` | 内核 |
| `tier_allows` / `required_tier` / `allows_builtin` / `allows_read` / `allows_write` / `allows_exec` | 内核 |
| `PermissionMode::ceiling()` / `effective()` / `min()` | 内核 |
| 「按档位决定注册哪些内置工具」 | `host.rs` |
| `/permission` 斜杠命令 | TUI |

**工具现在始终全量注册。** 档位曾经让"被禁工具在模型的 prompt 里物理不可见"；0.4 里模型**看得见** `write_file`，但 plan 模式会拒绝它并给出出口（§6）。这是"能力可见、准入可控"的取舍。

### 2.3 TS 插件

- `thunder-agent-plugin` **整包删除**（workspace members、`[workspace.dependencies]`、`root/Cargo.toml` 的 optional dep 与 `script-plugin` feature，含从 `default` 中移除）。
- 随包删除：`RunRegistry`、`RunServices`、`begin_run`、`set_tools`、`plugins/script_plugin.rs`、`plugins/standard.rs` 的 `script` 字段 / `with_script_plugin` / `has_ts_plugins`。
- `ReloadPlugins` RPC 改名 `ReloadIntegrations`（见 §1.1）。
- **能力对照**：Rust 插件保留 `ctx.ui()` 与 `ctx.tool_slot().invoke()`；**失去** `ctx.exec` / `ctx.fs` 的通用等价物（需要就自己实现 `AgentTool`），以及**动态发现、蓝绿热重载、第三方免编译扩展**三项。

---

## 3. 内核公共 API 收缩

`thunder-agent-loop` 从"带产品意见的运行时"回到"内核"。以下类型**移出**内核：

| 类型 | 0.3 位置 | 0.4 位置 |
|---|---|---|
| `Mode`（原 `PermissionMode`） | `loop::types::policy` | `thunder-agent-root::plugins::mode` |
| `AllowRule` | `loop::types::policy` | `thunder-agent-root::plugins::mode` |
| `ApprovalRequest` / `Verdict` | `loop::types::policy` | `GateVerdict` / `ApprovalPrompt`（`loop::types::gate`，纯数据结构） |
| `call_hash` / `ALLOW_*` 文案 | `loop::types::policy` | `ModePlugin` 内部 |
| `SessionPolicy` | `loop::types::policy` | **不存在**（职责由 `ModePlugin` 承担） |
| `Permission` | `loop::types::policy` | **不存在** |

`loop::types::policy` 现在只剩 `ToolEffect`、`Caller`、`describe_call`——它们是 mode 矩阵与 guard 的共同输入，属于通用语义。

新增的内核接缝（三个具名 trait，**顺序由内核写死**，没有"槽位/通用 hook"机制）：

| 接缝 | 签名要点 | 不变式 |
|---|---|---|
| `ToolScope` | `fn scope_of(&self, tool, effect, arguments) -> Scope` | 只返回分类，**不返回裁决**；默认 `Unknown` |
| `ToolGate` | `fn decide(&self, &ToolGateRequest) -> GateVerdict`；`fn record_answer(&self, &ToolGateRequest, &ApprovalAnswer)`；`fn label(&self) -> &str` | **只能收紧**；弹窗由内核负责 |
| `ToolTraceSink` | `fn record(&self, &ToolTrace)` | 只进不出 |

`ThunderPlugin` 新增 `fn tool_gate(&self) -> Option<Arc<dyn ToolGate>>`（默认 `None`）；`root` 的 `execute()` 在装配时把它转给 `AgentLoop::with_tool_gate`。

**`PluginContext.permission` → `PluginContext.mode`**，且 `policy()` 的 fallback **显式改成 `Yolo`**：若 fallback 成 `Ask` + `NullHostUi`（fail-closed），所有 Rust 插件会被静默锁死。

---

## 4. 软 jail：从"拒"到"问"

`pack-code/src/middleware/security.rs`（1594 行）删除。**删的是语义，不是解析器**：

- `normalize_path` / `resolve_for_check` **保留**，搬到 `pack-code/src/scope.rs`，成为 `CodeScope` 的分类助手。它是全仓唯一的符号链接逃逸检查。
- 危险命令判定换成 **token 判定**（`loop/src/tools/guard.rs`）：先分词（跟踪引号与转义），再只看首个动词与它的 operands。
- `is_forbidden_destructive_call`（旧 `policy.rs`，零调用者）与实际生效的 `security.rs` 清单是**同一份清单的两份副本**，现在合并成一处。

### 旧 jail 的误伤（以下**不再被拒**）

| 日常操作 | 0.3 被拒的原因（源码依据） | 0.4 |
|---|---|---|
| `rm -rf /tmp/build` | forbidden 是纯子串匹配，`"rm -rf /"` 命中 `rm -rf /tmp/…` | 放行/询问 |
| `grep -rn "mkfs" docs/`、`git commit -m "never run rm -rf /"` | 同上，引号里也照拒 | 放行 |
| `echo done > "$LOG"`、`rm "$tmpfile"` | 写目标以 `$` 开头 → fail-closed | 放行/询问 |
| `cd "$BUILD_DIR" && touch marker` | `cd` 到非字面量 → `Dir::Unknown`，其后任何相对写都拒 | 放行/询问 |
| `pushd src && … && popd && touch done` | `popd` 无条件置 `Unknown` | 放行/询问 |
| `python3 -c "import os; os.system('cat /etc/hosts')"` | `system(` 命中 `SCRIPT_WRITE_MARKERS` | 放行/询问 |
| `echo "error: wrote > /tmp/log"` | 引号完全不被追踪，引号里的 `>` 被当真重定向 | 放行 |
| `cmd 2> /dev/tty`、`script > /dev/console` | `/dev` 白名单只有 `null/stdin/stdout/stderr` | 放行/询问 |

### 反向：有漏的地方也不拦了

`find . | xargs rm -f` 因为 operand 来自 stdin 而**旧 jail 也没拦**。0.4 的 `bash` 一律返回 `Scope::Unknown`，不做静态目标分析。

### 仍然是**无条件拒绝**的

| 类别 | 判定 |
|---|---|
| 危险命令 | `mkfs*`；`dd if=`；`rm` 同时带递归与强制 flag 且 operand 是文件系统根；fork bomb |
| 宿主声明的 `Scope::Critical` | 凭据库（`auth.json`/`models.json`/`credentials.json`，双向）、thunder 数据根的写、`.git` 的写、系统目录（`/etc`、`/dev`、`/proc`、`/sys`、`/boot`、`/bin`、`/sbin`、`/usr`、`/var/db`、`/System`、`/Library`、`/Applications` …）的写 |

这两类**在任何模式（含 yolo）下都拒绝**，装一个"永远 Allow"的假 gate 也放不开。

### 各模式下的行为

| 场景 | plan | ask | acceptEdits | yolo |
|---|---|---|---|---|
| 工作区内的 `write_file` | 拒绝（并提示出口） | 询问 | **免问** | 免问 |
| 工作区外的写 | 拒绝 | 询问 | 询问 | 免问 |
| 任意 `bash` | 询问 | 询问 | 询问 | 免问 |
| 危险命令 / 关键路径 | 拒绝 | 拒绝 | 拒绝 | 拒绝 |

> **在无面板的宿主上，"询问"实际等于"拒绝"**（fail-closed）。所以放宽的部分只在交互式宿主上真正落地。

---

## 5. 权力放宽的诚实说明（R1）

**必须显式知道这一点，不能以为它悄悄消失了。**

0.4 的软 jail 相比 0.3 的硬 jail 是一次**净放宽**：

- 越界**不再硬拒**，而是变成询问；`yolo` 下**直接放行**。
- `bash` **不再做静态目标分析**。以下在 0.3 会被拒或至少被分析，在 0.4 的 `yolo` 下**静默执行**：
  - `tar -x -C /etc`、`make`（派生写）、`npm install`（postinstall 脚本）
  - 任何把命令拼进变量的写法：`CMD="rm -rf $DIR"; $CMD`
  - `python3 -c "…os.remove(…)"`、`perl -e …`、`node -e …`
- 旧 jail 的 best-effort 属性并没有消失，它只是换了位置：`bash` 的边界现在由**用户的批准**而不是 parser 决定。

**换来的是什么**：误伤消失，且新增判定**只能收紧到"问"，不能收紧到"拒"**——用户可以看见发生了什么并自己决定，而不是被一个 1594 行的启发式 parser 静默拦住。

**危险命令与关键路径这两类是收紧的**：token 判定消除了引号/子串误判，守卫不可被 gate 覆盖。

**建议**：把 `yolo` 当成"我知道自己在做什么"的显式选择，而不是默认。默认虽仍是 `yolo`（与 0.3 的行为连续性），但 `config.json` 的 `agent.defaultMode` 可以一次改掉：

```json
{ "agent": { "defaultMode": "ask" } }
```

---

## 6. plan 模式（新）

| 项 | 说明 |
|---|---|
| 计划文件 | `<workspace>/.thunder/plans/<session>.md`，`<session>` 消毒为 `[A-Za-z0-9._-]` |
| `write_plan` | 参数 `{content}`，整体覆盖；效果 `plan_artifact`；这是 plan 模式下**唯一可写**的东西 |
| `exit_plan_mode` | 无参数（忽略模型传的值），从磁盘读计划，弹 `UiRequest::Review` |
| 批准 | 三个落地模式：`Yes, and auto-accept edits` → `acceptEdits`；`Yes, but ask before each edit` → `ask`；`Yes, fully autonomous` → `yolo` |
| 驳回 | 留在 plan，再追问一次并把输入作为 tool result 回灌模型（**不是错误**） |
| 无面板 | 计划永不获批，会话留在 plan——沉默不是同意 |
| 豁免条件 | 写死**工具名**，不是效果：自称 `PlanArtifact` 的工具照样被拒 |

新的 UI 请求类型：`UiRequest::Review { title, path, content, options, timeout_ms }`（wire 上 `ui: "review"`）。宿主需要为它加一个分支；TUI 与 daemon 都已实现。

TUI 的操作方式：`Shift+Tab` 循环模式（当命令行参数补全未激活时），状态栏常驻模式徽标。

---

## 7. 两处保活的线上契约

### 7.1 `AgentEvent::Custom { kind: "file_change" }` **仍然发出**

中间件删掉了，但事件的**发出下移**到工具层：`write_file` 的变更报告并入工具自身，`bash` 的 `git status --porcelain` 差分搬进 `BashTool::execute`。**事件形状与 `kind` 不变**，payload 的键不变。

附带收益：被拒绝的 `bash` 不再白跑两次 `git status`。

> ⚠️ **"契约保活"不等于"你的代码是对的"**：线上形状自 0.3.0 起就是 `{"type":"custom","kind":"file_change","payload":{path,action,bytes,tool}}`——`type` 是 **`custom`**，字段在 **`payload`** 里且叫 **`tool`**。按 `type === "file_change"` + `tool_name` 匹配的消费方（ARP 就是，见 §8.1）在 0.3 上就已经收不到它了。0.4 只是继续原样发出。

### 7.2 不变的东西（逐条）

| 契约 | 状态 |
|---|---|
| `AgentEvent` 既有变体名与字段名 | **不变**（`ToolTrace` 是**新增**变体；ARP 的 TS 联合有 catch-all，PS 的 switch 落空即忽略） |
| `FinishReason` 的**线上拼写** | **不变**。daemon 用 `format!("{:?}", …)`（**Debug**）塞进 `TaskCompleted.finish_reason: String`，消费方比对的是 `"Error"`/`"Done"`/`"MaxTurnsExceeded"` 这类 **Rust 变体名**——既不改变体名，也不改名成 serde 序列化 |
| `ToolEffect` 的线上拼写 | **不变**：`read` / `write` / `exec`（新增 `plan_artifact`，是增量） |
| trace 文件既有字段与 `events[]` 元素形状 | **不变**（只能新增字段） |
| crate 名、二进制名、目录名 | **不变**（`thunder-agent-daemon`、`thunder-tui`、`thunder-agent-core/tui`、`daemon.sh`/`run.sh`、`target/{release,debug}`） |

### 7.3 trace 文件新增的字段

`<store>/<session>/traces/<task_id>.json` 追加：

| 新字段 | 内容 |
|---|---|
| `tool_traces[]` | 每次工具调用的完整记录（上限 2000） |
| `tool_decisions[]` | 被上限挤出的记录，保留"为什么允许/拒绝"（上限 2000） |
| `dropped_deltas` | 被丢弃的流式增量**计数**（原来静默丢弃） |
| `dropped_events` | 超出 5000 上限的事件计数 |

`ToolTrace` 的关键字段：`trace_id`、`task_id`、`turn`、`tool_call_id`、`tool`、`caller`、`effect`、`mode`、`scope`、`arguments`（**已脱敏**）、`decision`（`allow`/`allow_by_rule`/`asked`/`deny{reason, by}`/`cancelled`）、`guard`、`answer`、`result`、`redactions`、`timing`。

**脱敏默认开启**：密钥键名、密钥形态（`ghp_…`/`sk-…`/`AKIA…`/JWT/PEM/Bearer/URL 内嵌凭据）、`content`/`preview` 正文（只留 digest + 200 字符预览）、工具输出预览（2048 字符 + 全文 digest）。**模型上下文不受影响**——模型需要真相才能干活。

---

## 8. 消费方要改什么

### 8.1 agent-resume-panel (ARP)

| 维度 | 事实 | 0.4 是否受影响 |
|---|---|---|
| 二进制 | 无 pin，打包时从 checkout 现场 `cargo build -p thunder-agent-daemon` / `-p thunder-tui` | 无（crate/目录名未变） |
| 数据根 | 直接读写 `~/.thunder`；不支持 `THUNDER_CONFIG_DIR` | 无新增要求（`conversations/` 布局未变） |
| 模型标识 | 发 `model` = `selection_id` 字符串 | 无 |
| trace | 既走 RPC 又解析磁盘文件，逐字段读 `task_id`/`started_at_ms`/`duration_ms`/`prompt`/`wall_duration_ms`/`stats.*`/`events[]` | **字段全部保留**，只会看到四个新键（`as any` 下安全） |
| 模式 / tier / 插件 | **零交互**（`active_plugins` 仅透传） | 无；若日后要暴露模式，用 `SetMode`/`GetPermissionState` |

**要做的**：① 若关心协议修订，读 `ping` 的 `protocol`；② 其余零改动。**不要**顺手改 crate/目录名——ARP 硬编码了它们。

**另有一个与 0.4 无关的既有缺陷，升级时顺手修掉**：ARP 匹配 `ev.type === "file_change"` 且读 `ev.tool_name`（`apps/desktop/src/main/thunder/thunderProtocol.ts:78`、`apps/desktop/src/renderer-react/features/chat/useTraceCollector.ts:243,437`）。但如 §7.1 所述，自 thunder 0.3.0 起这个事件的 `type` 就是 **`custom`**、字段在 `payload` 里且叫 **`tool`** —— 也就是说 ARP 的实时「文件变更」popover 从 0.3 起就一直是空的，现在看到的记录来自 `useThunderChat.ts` 对会话 `tool_executions` 的回填（`write_file` 那条路径），`bash` 的变更根本进不来。修法是消费侧两行：`ev.type === "custom" && ev.kind === "file_change"`，字段读 `ev.payload.tool`。**PS 写对了**（`agent-bridge.ts:1179`），可作参照。

### 8.2 prototype-edit-llm (PS)

pin 需从 `0.3.0` / `3faae1b` 升到 `0.4.0` 并更新 sha256。四条活契约：

| 契约 | 0.4 状态 |
|---|---|
| `AgentEvent::Custom{kind:"file_change"}` 触发原型预览同步 | **保留**（发出下移到工具层，形状不变） |
| `tool_exec_result` 里 `name == 'write_file' \|\| 'bash'` 触发同步 | **保留** |
| `AgentEvent` 既有变体名与字段名 | **保留**（只新增 `ToolTrace`） |
| `FinishReason` 字符串被正则解析 | **保留**（线上拼写不变） |
| `effect` 字面量 `'read'\|'write'\|'exec'` 硬编码 | **保留**（线上拼写不变） |

PS **不装 gate**，所以这些工具必须继续放行——这正是"不装 gate ⇒ 除守卫外全放行"这条红线存在的原因。**要做的**：只升 pin + 冒烟验证 `file_change` 预览同步与失败态判定。

其余与本次无关但记录在案：PS **写** `<data_root>/models.json`（utility model，`apiKey` 明文 + 0600），并无条件设 `THUNDER_CONFIG_DIR=~/.prototype-studio/thunder`。

---

## 9. 版本

- workspace 版本：`0.3.0` → **`0.4.0`**。
- daemon `protocol`：**2**；runtime `PROTOCOL_VERSION`：**`"2"`**。
- 旧模式名仍可解析，旧 RPC `ReloadPlugins` 需改名 `ReloadIntegrations`。
