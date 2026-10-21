[English](README_en.md) | [简体中文](README.md)

# thunder-agent-types

## 定位

Thunder 的线格式词汇表：agent loop 与各宿主共享的消息、事件、错误、通知、工具声明与结果、策略声明、作用域与 trace 记录。

本 crate 只有数据与 trait 声明。没有 I/O，没有执行，没有判断——引入它不会把 loop 或执行层一起拖进来。工具包、UI、嵌入方都可以只依赖它。

## crate 边界

外部依赖只有 `serde`、`serde_json`、`async-trait`。

下游（`Cargo.toml` 中声明 `thunder-agent-types`）：`thunder-agent-loop`、`thunder-agent-policy`、`thunder-agent-runtime`、`thunder-agent-providers`、`thunder-agent-skills`、`thunder-agent-mcp`、`thunder-agent-pack-code`、`thunder-agent-root`、`thunder-agent-daemon`、`thunder-agent-core/tui`。

"零内部依赖"是硬约束，不只是洁癖：kernel（`thunder-agent-loop`）与 policy（`thunder-agent-policy`）都依赖本 crate。若本 crate 反向依赖它们，就形成依赖环，Cargo 无法构建。因此这里只能出现双方都需要的名词；任何"谁来决定"的逻辑必须留在上层。

## 模块

| 模块 | 内容 |
|---|---|
| `message` | `Role`、`ToolCall` / `ToolCallFunction`、`ContentPart`、`ChatMessage`（system / user / assistant / tool） |
| `event` | `AgentEvent`、`ObservedEvent`、`TurnStats`、`AgentStats`、`FinishReason`、`EventDropCounters` |
| `tool` | `ToolDefinition` / `FunctionDefinition`、`ToolExecutionResult`、`ToolEventSink`、`ProgressReporter` |
| `policy` | `ToolEffect`、`Caller`、`describe_call` —— 描述调用"是什么"，不含"是否允许" |
| `scope` | `Scope`、`ToolScope`、`UnknownScope` |
| `notice` | `SystemNotice` 及其 Markdown 渲染 |
| `trace` | `ToolTrace`、`Decision`、`DenyBy`、`GuardHit`、`GuardKind`、`AskAnswer`、`TraceResult`、`TraceTiming`、`ToolTraceSink`、`TRACE_SCHEMA` |
| `error` | `AgentError`：可恢复的单元级失败 |

## 核心类型

### 消息

```rust
ChatMessage::System { content, name? }
ChatMessage::User { content, parts?, name? }
ChatMessage::Assistant { content: Option<String>, tool_calls?, refusal?, name?, reasoning?, stats? }
ChatMessage::Tool { tool_call_id, content, name? }
```

- `Assistant.content` 始终序列化（`null` 也写出），满足 OpenAI/DeepSeek 的请求格式；
- `User.parts` 仅在有附件时出现，纯文本消息与原线格式逐字节一致；
- `ContentPart::Image { mime_type, data, name?, path?, sha256? }`：`data` 是 base64（不带 `data:` 前缀），`path`/`sha256` 是会话存储卸载字节后的持久化引用；
- `ChatMessage::assistant_turn(...)` 是"一轮一条消息"的持久化形态，携带该轮的 `reasoning` 与 `TurnStats`。

### 工具

| 类型 | 说明 |
|---|---|
| `FunctionDefinition { name, description, parameters, strict? }` | 模型可见的函数声明；`parameters` 是 JSON Schema |
| `ToolDefinition { tool_type, function }` | `tool_type` 固定为 `"function"` |
| `ToolExecutionResult { output, is_error, truncated, original_bytes, duration_ms, telemetry? }` | 工具交回的结果；`telemetry` 是模型可见的 `SystemNotice` |
| `ToolEventSink::emit_custom(kind, payload)` | 业务自定义事件出口，`payload` 对 kernel 不透明 |
| `ProgressReporter::report_progress(...)` | 长任务进度回调 |

`ToolExecutionResult::success` / `error` 都把 `original_bytes` 设为 `output.len()`；`with_telemetry` 会把通知渲染成 Markdown 追加进 `output`（输出为空时直接作为正文），并同步刷新 `original_bytes`。

### 事件

`AgentEvent` 以 `#[serde(tag = "type", rename_all = "snake_case")]` 编码，变体：

`TurnStart`、`TokenDelta`、`ReasoningDelta`、`ToolCallChunk`、`ToolCallReady`、`ToolExecStart`、`ToolExecResult`、`ToolTrace`、`TelemetryNotice`、`TurnEnd`、`LoopComplete`、`ContextCompacted`、`SteerAccepted`、`Error`、`Custom`、`GateResult`。

`ObservedEvent` 给事件加宿主侧路由信息：

| 字段 | 含义 |
|---|---|
| `agent_id` | 发出该事件的执行单元，调度器据此解复用多个 agent |
| `event` | `AgentEvent` |
| `timestamp_ms` | loop 发出事件的墙钟时间；`#[serde(default)]`，旧 trace 反序列化为 `0` |

`ToolTrace` 事件带盒子（`Box<ToolTrace>`）：trace 是这条线上最大的变体，而事件每个订阅者都要 clone 一次。

### 策略与作用域

`ToolEffect` 按工具名分类，未注册的名字落到 `Other`（行为等同 `Write`）——不认识的工具绝不假设无害。

| 效果 | 判定（`ToolEffect::of`） |
|---|---|
| `read` | `read_file` `grep` `find` `ls` `list_dir` `read` `glob` `exit_plan_mode` |
| `write` | `write_file` `edit` `write` `apply_patch` `notebook_edit` |
| `exec` | `bash` `shell` `powershell`，以及名字含 `exec` / `shell` / `terminal` / `powershell` / `command` 或以 `_bash` 结尾的工具 |
| `plan_artifact` | `write_plan` |
| `other` | 其余全部 |

序列化拼写（`read` / `write` / `exec` / `plan_artifact` / `other`）是线格式契约，trace 与远端工具声明共用。

`Caller`（内部标签 `caller`）区分 `Model` 与 `Plugin(String)`：审批对话框必须能说明是插件在请求，否则"用户同意"会被误当成同意模型。

`Scope` 是一个**位置**，不是判决：

| 变体 | 含义 |
|---|---|
| `InScope` | 落在宿主声明的工作区内 |
| `OutOfScope` | 在外；本身不构成拒绝，只是不获得自动放行 |
| `Critical` | 禁区（如 `.git`、凭据库）：kernel 在任意模式下无条件拒绝且不询问 |
| `Unknown`（默认） | 宿主无法判断：不获得自动放行，于是走询问 |

`ToolScope::scope_of(tool, effect, arguments) -> Scope` 由宿主实现，同步且必须全函数：无法判断就返回 `Unknown`，不能失败。`UnknownScope` 是"什么都不知道"的实现，供测试与无工作区的宿主使用。

注意方向性：scope 只能**放宽**（`acceptEdits` 下工作区内的写入无需弹窗），永远不能把 gate 本会允许的调用变成拒绝；唯一例外是 `Critical`，它是宿主明说"这里不该问，这里是禁区"。

`describe_call(args)` 是审批文案的唯一来源：优先 `command`、其次 `path`、否则紧凑 JSON，均截断到 200 字符。

### 通知、trace 与错误

- `SystemNotice { layer, action, ground_truth, self_healed?, guidance? }` 是**模型可见**的遥测半边，讲清系统替模型做了什么、真实状态如何；人类可见的另半边是 `ToolTrace`。二者刻意分离：模型需要事实以继续工作，审计工件需要脱敏与结构。
- `ToolTrace` 是**人类/持久化可见**的调用记录，字段见 `thunder-agent-types/src/trace.rs`；`TRACE_SCHEMA = 1`，读方应按 `schema` 分派。`Decision` 区分 `Allow` / `AllowByRule` / `Asked` / `Deny { reason, by }` / `Cancelled`；`DenyBy` 区分 kernel 的 `Guard` 与宿主策略的 `Gate`。
- `ToolTraceSink::record(&ToolTrace)` 单向出站：kernel 交出记录后即遗忘，sink 收到的内容已脱敏。
- `AgentError` 只表示可恢复的单元级失败（`AlreadyRunning`、`Terminated`、`Config`、`Other`）。取消、预算、最大轮数这类终态属于 `FinishReason`，不是错误。

## 事件契约：`approx_bytes` 与 `EventDropCounters`

事件队列有字节预算（loop 侧 `AgentConfig::max_event_bytes`，默认 8 MiB），每次入队按 `ObservedEvent::approx_bytes()` 计费。

`approx_bytes` 是**估算，不是序列化**：

```
96（固定帧：判别符、时间戳等固定开销） + agent_id.len() + payload
```

`payload` 只统计会真正增长的部分：文本 delta 取 `len()`；`ToolCallChunk` 取 id/name/arguments_delta 的长度；`ToolCallReady` 取 id/name/arguments；`ToolExecStart` 取 name + `arguments.to_string().len()`；`ToolExecResult` 取 `result.output.len()`；`ToolTrace` 取 `serde_json::to_string` 长度；`TelemetryNotice` 取各字符串长度；`LoopComplete` 取 `final_content`；`SteerAccepted` 取 `message`；`Error` 取 `message`；`Custom` 取 kind + payload；`GateResult` 取 verdict。未列出的变体 payload 记为 0，只收固定帧。

对每个 token delta 跑一遍 `serde_json` 的成本高于这个界本身省下的内存，所以刻意估算。

`EventDropCounters`（`AtomicU64`，`Relaxed`）把"投递失败"变成可观测事实，分两类：

| 计数 | 触发 | 语义 |
|---|---|---|
| `micro_deltas` | `TokenDelta` / `ReasoningDelta` / `ToolCallChunk` 在通道满时被 `try_send` 丢弃 | 流式增量缺失，正文可由最终消息补齐 |
| `structural` | 生命周期事件与业务事件（含 `Custom`）在有限等待后仍未投递，或单条事件超过整个预算（`TurnEnd`、`LoopComplete`、trace 等） | 本次运行的记录可能不完整 |

`EventDropCounters::shared()` 在 emitter、sink 与宿主之间共享同一组计数；`total()` 是两者之和。宿主应把 `structural() > 0` 视为"运行记录可能不完整"的信号，而不是可忽略的指标。

## 默认值与线格式常量

| 项 | 值 |
|---|---|
| `ObservedEvent::approx_bytes` 固定帧 `FRAME` | 96 B |
| `TRACE_SCHEMA` | 1 |
| `describe_call` 截断 | 200 字符 |
| `AgentEvent` 判别标签 | `"type"`，`snake_case` |
| `ObservedEvent.timestamp_ms` 缺省 | 0 |
| `Scope` 默认 | `unknown` |
| `ToolEffect` 未知名字 | `other` |
| `FinishReason` 拼写 | `done` / `max_turns_exceeded` / `budget_exceeded` / `cancelled` / `error` |

## 失败方向

- 本 crate 不产生运行时失败，只有数据。反序列化的宽松点是刻意的：`timestamp_ms` 与新增可选字段都有 `#[serde(default)]`，旧 trace 仍可读。
- 缺省值一律选安全侧：`Scope::Unknown` 不获得自动放行；未分类的工具效果是 `Other`（按 `Write` 对待）；`Caller::Model` 之外的调用必须显式标注。
- `AgentError` 不承载终态；把取消/预算/轮数上限当成 `AgentError` 会误导调用方重试。

## 测试

10 个内联单元测试，覆盖线格式而非行为：

- `event.rs`：旧事件缺 `timestamp_ms` 时反序列化为 0；`tool_trace` 变体以自己的标签往返。
- `message.rs`：纯文本 user 消息不产生 `parts` 键；多模态往返保留 `mimeType`/`data` 且不写空的 `path`/`sha256`；空 `parts` 收敛为普通消息。
- `notice.rs`：`format_markdown` 的字段顺序与可选段落。
- `policy.rs`：未知工具不视为无害；`read`/`exec`/`plan_artifact` 的序列化拼写。
- `scope.rs`：`Scope::default()` 是 `Unknown`、`UnknownScope` 恒答 `Unknown`；`in_scope` / `out_of_scope` 的线上拼写。

## 扩展点

- 实现 `ToolEventSink`：让工具发出宿主定义的事件（落为 `AgentEvent::Custom`）。
- 实现 `ProgressReporter`：接入长任务的进度 UI。
- 实现 `ToolScope`：告诉 kernel 调用落在哪里。
- 实现 `ToolTraceSink`：把 trace 落到自己的存储。
- 新增自有事件：使用 `AgentEvent::Custom { kind, payload }`，kernel 不解释 `kind`；未知 kind 对宿主是安全可忽略的。
