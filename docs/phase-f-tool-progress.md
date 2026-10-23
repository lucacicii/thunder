# Phase F 方案：插件自报进度信号（`ToolOutcome`）

> 状态：**暂缓（deferred）** — 方案已定，暂不实施，等真实工作流证明 bash 盲区确实咬人再做
> 前置：`docs/agent-loop-budget-and-recovery.md`（Phase A–E 已落地）、`docs/known-issues.md` KI-9
>
> 暂缓理由：两轮真机任务的 22 次调用全部 `novel=true`、`peak_no_novel_streak` 全为 0，
> 说明当前样本里 bash 的盲区还没有造成误判。F1+F2 的收益是「覆盖 `sed -i` / `git checkout`
> 这类用 shell 改文件的场景」，属于提高检测精度而非补齐能力。等样本攒够、或真的观察到
> bash-heavy 流程漏检，再从这里接着做——方案无需重做，直接按 §7 F1→F2 实施即可。

---

## 0. 要解决的问题

停滞检测现在对 `Neutral` 类工具（`bash`、第三方 MCP）只有一条判据：**输出摘要**。

```
fingerprint(Neutral, tool, args, output) = hash(tool, output)
```

后果（KI-9）：

| 场景 | 现状 |
|---|---|
| 模型用 `bash sed -i` / `git checkout` 改文件 | args 无法刻画副作用，`mutation_free_streak` 视而不见 |
| `bash cargo test` 输出带耗时/序号 | 每次都「新」，`no_novel_streak` 永远归零 → 停滞检测失效，只剩 `wall_clock` 兜底 |
| 输出恒定的重试 | 能抓到（这条是唯一有效的） |

Phase F 让插件**在结果里额外回传一个结构化进度信号**，把「我改了世界」和「我学到了新东西」这两件事从 core 的猜测变成插件的声明。

### 非目标（必须说清楚）

- **第三方 MCP server 默认不会自报。** 这是非标准扩展字段，不改造的 server 保持今天的 `Neutral` 行为。Phase F 对它们的价值是「提供了一条可选通道」，不是「自动修好」。真正被修好的是 `bash`（in-repo）。
- 不改变写入 LLM 上下文的内容。`ToolOutcome.text` 就是今天的那个 String。

---

## 1. 现状的三个事实（决定了方案形状）

1. `ToolProvider::call_tool` 的返回值是**整个插件→core 数据平面**，一个裸 `String`（`thunder-protocol/src/schema.rs:312`）。
2. `McpCallToolResult`（`thunder-protocol/src/mcp.rs:161`）只有 `content` + `is_error`，没有 `_meta` / `structuredContent`，所以跨进程也传不了额外信息。
3. 桌面端**早就有** `AgentEvent::Custom{kind:"file_change"}` 的渲染分支（`useTraceCollector.ts:255`），但全仓库没有任何 Rust 代码产生它 —— 通道设计过、从未实现。

→ 所以 Phase F 要**先造通道，再喂数据**。第 3 点顺便免费复活。

---

## 2. 协议层（`thunder-protocol`）

```rust
// schema.rs

/// A tool's own answer to "did this call change anything?".
///
/// The kernel treats the digests as **opaque tokens**: it only compares them for
/// equality, never interprets or recomputes them. That is what lets a plugin running
/// in another process report progress at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressSignal {
    /// On-disk state changed. `paths` is advisory (for UI); `digest` is the identity.
    Mutated { digest: u64, #[serde(default)] paths: Vec<String> },
    /// New information was gathered without changing state.
    Observed { digest: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    /// Exactly what goes into the LLM context today.
    pub text: String,
    pub progress: Option<ProgressSignal>,
}

impl ToolOutcome {
    pub fn from_text(text: impl Into<String>) -> Self { Self { text: text.into(), progress: None } }
}
impl From<String> for ToolOutcome { /* … */ }
impl From<&str> for ToolOutcome { /* … */ }
```

Trait 用**默认方法**扩展，零 blast radius：

```rust
#[async_trait]
pub trait ToolProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError>;
    async fn call_tool(&self, name: &str, arguments: Value, ctx: &ExecutionContext)
        -> Result<String, ToolExecutionError>;

    /// Optional richer entry point. Defaults to `call_tool` with no signal, so every
    /// existing provider keeps working untouched.
    async fn call_tool_with_progress(&self, name: &str, arguments: Value, ctx: &ExecutionContext)
        -> Result<ToolOutcome, ToolExecutionError>
    {
        self.call_tool(name, arguments, ctx).await.map(ToolOutcome::from_text)
    }
}
```

选择默认方法而不是改签名的理由：Phase F 是「可选增强」，改成 `-> Result<ToolOutcome, _>` 会强迫 7 个 provider + 一堆测试同时改，收益为零。

**跨进程通道**（`mcp.rs`）：给 `McpCallToolResult` 加一个可选字段。

```rust
pub struct McpCallToolResult {
    pub content: Vec<McpContentBlock>,
    #[serde(default)]
    pub is_error: bool,
    /// Non-standard extension: a tool may report its own progress signal.
    /// Servers that do not send it are treated exactly as today (`Neutral`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressSignal>,
}
```

---

## 3. 传播层

| 位置 | 改动 |
|---|---|
| `ToolRegistry` | 新增 `call_with_snapshot_outcome(...) -> Result<ToolOutcome, ToolExecutionError>`；旧 `call_with_snapshot` 保留（转调新方法后取 `.text`），避免动现有测试 |
| `mcp_client.rs:486` | `call_tool_with_progress` 覆盖：解析 `McpCallToolResult.progress` → `ToolOutcome` |
| `agent_loop::dispatch_single_call` | 返回 `(ToolExecutionResult, Option<ProgressSignal>)`；meta-tool 路径与 middleware 拒绝路径给 `None` |
| 其余 provider（`thunder-plan` / `thunder-ask` / 脚本工具） | 不动 |

---

## 4. 消费层（`progress.rs`）

插件信号优先，缺失时回退到今天的 `ProgressKind` 推断：

```rust
pub fn record_plugin(&mut self, signal: &ProgressSignal) -> bool {
    match signal {
        ProgressSignal::Mutated { digest, .. } => {
            let fp = mix(PLUGIN_DOMAIN, *digest);   // 域分隔：绝不与 core 自算指纹相撞
            let novel = self.mutations.insert(fp);
            if novel { self.total_mutations += 1; self.turn_had_mutation = true; }
            self.results.insert(fp);
            if novel { self.total_novel_observations += 1; self.turn_had_novel_output = true; }
            novel
        }
        ProgressSignal::Observed { digest } => { /* 同理，只进 results */ }
    }
}
```

`agent_loop` 里：

```rust
if let Some(signal) = plugin_signal {
    self.progress.record_plugin(&signal);
    if let ProgressSignal::Mutated { paths, .. } = &signal {
        for path in paths.iter().take(MAX_FILE_CHANGE_EVENTS) {
            self.emit_agent(AgentEvent::Custom {
                kind: "file_change".into(),
                payload: json!({ "path": path, "action": "modified", "tool": call.name }),
            });
        }
    }
} else {
    self.progress.record(progress_kind, &call.name, &args_serialized, &result_text);
}
```

⚠️ `paths` 来自**不可信输入**（第三方 server 可以随便写）。只做三件事：截断长度（≤512 字符）、限制条数（≤10）、当字符串原样转发。**绝不用它去访问文件系统。**

---

## 5. `bash` 怎么知道自己改了什么

这是整个 Phase F 最实质的部分。`bash` 只有 args（命令行字符串），无法静态推断副作用。

### 方案：工作区指纹差分（gitignore 语义）

```
before = workspace_digest(cwd)      // 执行前
run the command
after  = workspace_digest(cwd)      // 执行后
if after.digest != before.digest:
    Some(ProgressSignal::Mutated { digest: after.digest, paths: after.changed_vs(&before) })
else:
    None                            // 明确「什么都没变」，比 Neutral 的猜测强
```

`workspace_digest` 的实现要点：

1. **只走 gitignore 可见的文件**（复用 `thunder-std` 已依赖的 `ignore` crate）。
   → `cargo build` 写 `target/` 数千个文件时**不算** mutation。这不是妥协：构建产物不是「源状态进展」，而人类语义下的进展恰恰是源文件。
2. **只取 `(path, mtime, size)`**，不读内容 → 一次 walk 是纯 stat。
3. **变化文件的内容哈希**：`before`/`after` 差分出变化路径后，只对这些文件读内容做最终 digest（通常 0–5 个文件），这样「同 mtime 同大小但内容变了」也不会漏。
4. **上限与降级**：条目数 > `max_entries`（默认 20k）或耗时 > `budget`（默认 300ms）→ 放弃本次差分，返回 `None`（退回今天的 `Neutral` 行为）。**绝不能因为统计把 bash 拖慢。**

放 `thunder-executor/src/workspace.rs`（sandbox 工具层），`thunder-std` 的 bash handler 调用。core 完全不知道「文件」概念。

### 成本（实测，本仓库）

| 范围 | 文件数 | find+stat 耗时 |
|---|---|---|
| 仅源文件（排除 target/.git） | 177 | 51ms（其中大头是进程启动） |
| 忽略规则后（同 `bash cargo build` 场景） | ~177 | 73ms |
| 含 `target/` 全量 | 7,715 | 220ms |

进程内 Rust walker 无进程启动开销，预期 **1–10ms（源树）/ 数十 ms（异常大的树）**。F2 落地后必须补一次进程内实测，把数字写回本文档。

---

## 6. 与现有 `ProgressKind` 的关系

| provider | 声明 | 行为 |
|---|---|---|
| `write_file` / `edit_block` | `Mutating` | 保持现状（core 用 args 指纹，精确）。**可选**额外报 `paths` 以点亮 `file_change` 事件（F3） |
| `read_file` / `grep` / `find` / `ls` | `Observing` | 保持现状 |
| `bash` | `Neutral` | **F2 后自报** `Mutated{digest, paths}` 或 `None`；无信号时仍按输出指纹 |
| 脚本工具 | 默认 `Neutral`（可在 frontmatter 声明） | 同 bash，但脚本自己不知道副作用 → 保持现状。若将来给脚本套同一层 workspace 差分，可复用 |
| 第三方 MCP | `Neutral` | 只有它主动发 `progress` 字段时才有信号 |

优先级：**插件信号 > descriptor 声明 > 输出指纹**。

---

## 7. 分阶段

| 步骤 | 内容 | 行为变化 | 预估 |
|---|---|---|---|
| **F1** | `ProgressSignal` / `ToolOutcome` / 默认方法 / registry 直通 / core 消费 + 域分隔；fake provider 测试 | 无（现有工具都走回退） | 半天 |
| **F2** | `workspace_digest` + `bash` 自报；上限/降级；进程内耗时实测 | bash-heavy 流程的停滞检测从「靠输出」变「靠状态」 | 1 天 |
| **F3（可选）** | `write_file`/`edit_block` 报 `paths`；core 发 `file_change` 事件（复活桌面端死代码） | 纯展示 | 半天 |

建议先只做 F1+F2：F3 是 UI 收益，与检测正确性无关，等面板那边确认要再动。

---

## 8. 测试计划

**F1**
- fake provider 覆盖 `call_tool_with_progress` 返回 `Mutated`：同 args 同 digest 两次 → 只算一次进展；不同 digest → 两次
- 不覆盖 `call_tool_with_progress` 的 provider → 指纹与今天逐字节一致（回归）
- 域分隔：插件 digest 与 core 自算指纹同值时不相撞
- 跨进程：`McpCallToolResult` 带 `progress` 时经 `thunder-std` 往返后仍存在；不带时字段缺失且解析正常

**F2**
- `bash "echo hi > f.txt"` → 有 `Mutated`，`paths` 含 `f.txt`
- `bash "ls"` / `bash "true"` 连跑两次 → 第二次 `None`
- `bash "sed -i …"` 改内容但大小不变 → 仍检测到（内容哈希兜底）
- `bash "cargo build"` 只写 `target/` → `None`（gitignore 语义）
- 大目录（> max_entries）→ `None` 且不 panic、不超时
- 回归：未装 gitignore 的目录（无 `.gitignore`）也能工作

---

## 9. 风险与取舍

1. **每次 bash 两次 walk**：有上限 + 降级，但仍是新增 IO。若真机上出现可感知延迟，退化为「只在 `mutation_free_streak >= 2` 时才开启差分」的自适应开关。
2. **gitignore 语义的偏差**：未纳入 git 的文件（如 `!important.log` 被忽略）不计入进展。可接受：模型改的通常是源码。
3. **第三方 MCP 仍不会自报**：KI-9 只被部分解决，文档必须说清楚。
4. **`paths` 是不可信输入**：只转发不落地，见 §4。
5. **digest 只在「相等性」上有意义**：跨进程/跨版本的 hash 稳定性无关紧要，不要写进契约。

---

## 10. 待拍板

1. `workspace_digest` 默认**开**还是关？（我倾向默认开 + `sandbox.workspace_digest = true` 可关，因为它是 bash-heavy 流程唯一的检测手段）
2. 上限取值：`max_entries = 20_000` / `budget = 300ms` 是否合适？
3. F3（`file_change` 事件）现在就做，还是等面板确认？
