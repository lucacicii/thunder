[English](README_en.md) | [简体中文](README.md)

# thunder-conversation

会话模型与持久化层：把「一轮对话」定义成可序列化的数据结构，并把「什么算一个会话」这条规则写死在存储里。

## 定位

`thunder-agent-loop` 只认识一次运行（`ContextInput` / `AgentRunResult` / `ChatMessage`），不负责跨运行的会话。本 crate 补上这一层：

- `Conversation`：消息列表 + 元数据 + 统计，是磁盘上的会话实体。
- `ConversationStore`：读写抽象的 trait，`MemoryConversationStore` 与 `FsConversationStore` 两个实现。
- `ConversationManager`：在 store 之上的 CRUD / fork / archive / resume。
- `Turn` 视角：`extract_turns` / `truncate_turns`，给 UI 的「第 N 轮」和「只保留最近 N 轮」提供统一口径。
- `OrchestrationMeta` / `StageRecord`：多智能体编排的留痕。
- `ConversationExporter`：Markdown / JSON / OpenAI 消息三种导出。

## crate 边界

内部依赖（`Cargo.toml`）：

| 依赖 | 用途 |
|---|---|
| `thunder-agent-loop` | `ChatMessage` / `Role` / `ToolCall` / `ContentPart`、`ContextBuffer`（token 估算）、`AgentRunResult`、`paths::thunder_subdir` |

除标准库外只用 `tokio`、`serde`/`serde_json`、`async-trait`、`tracing`。不依赖 `thunder-agent-policy` / `thunder-agent-root`：本 crate 不知道工具、模型与宿主的存在。

依赖本 crate 的成员：

| 成员 | 方式 |
|---|---|
| `thunder-agent-root` | 可选依赖，feature `conversation`（默认开启）；`plugins/conversation.rs`、`plugins/standard.rs` 使用 `ConversationStore` |
| `thunder-agent-daemon` | 直接依赖，`service.rs` 持有会话 |
| `thunder-agent-core/tui` | 直接依赖，`App` 的持久化与标题生成 |

## 核心类型

`Conversation`（`src/types.rs`）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | `String` | 会话 ID，同时是磁盘目录名 |
| `title` | `Option<String>` | 显示名；占位标题可被替换 |
| `title_source` | `Option<String>` | `"auto"`（模型生成）或 `"manual"`（用户设定） |
| `parent_id` | `Option<String>` | fork / 分支来源 |
| `system_prompt` | `Option<String>` | 与 `messages[0]` 保持一致的 system 消息 |
| `model` / `workspace` / `thinking_level` | `Option<String>` | 运行期绑定，跨运行保留 |
| `shared_roots` | `Vec<String>` | 与工作区同权限的附加根目录 |
| `status` | `ConversationStatus` | `Active`（默认）/ `Archived` / `Deleted` / `Paused` |
| `messages` | `Vec<ChatMessage>` | 权威消息序列 |
| `stages` | `Vec<StageRecord>` | 编排阶段记录 |
| `orchestration` | `Option<OrchestrationMeta>` | 拓扑、角色、run_id、路由原因 |
| `metadata` | `HashMap<String, serde_json::Value>` | 自由字段；`tags` 数组会被 `to_summary` 读出 |
| `stats` | `ConversationStats` | 见下 |
| `created_at_ms` / `updated_at_ms` | `u64` | 毫秒时间戳（`now_ms()`） |

`ConversationStats` 有两个不同口径的 token 字段，不可互换：

- `total_tokens`：当前工作上下文的估算值，每次 `touch()` 由 `ContextBuffer::calculate_message_tokens` 重算。
- `total_used_tokens`：累计计费量（provider 上报，含 cache 读写），从不从文本重算，只由宿主累加。

其余字段：`message_count`、`turn_count`（用户消息数）、`tool_calls_count`、`duration_ms`。

构造与变更方法（同文件）：

| 方法 | 行为 |
|---|---|
| `new(id)` | `Active`、空消息、`created_at_ms = updated_at_ms = now` |
| `with_title` / `with_parent_id` / `with_model` / `with_workspace` / `with_thinking_level` / `with_orchestration` | 构建期设置 |
| `with_system_prompt` | 写入或替换 `messages[0]` 的 system 消息并重算统计 |
| `bind_workspace_if_empty` | 仅当 `workspace` 为空时绑定 |
| `add_user_message` / `add_user_message_with_parts` | 追加用户轮；若 `title` 为 `None`，取内容前 40 个字符作标题 |
| `add_assistant_message` / `add_assistant_turn` / `add_tool_message` / `add_message` | 追加并 `touch()` |
| `append_agent_result` | 合并 `AgentRunResult.messages`（已存在 system 时跳过其首条 system），累加 `duration_ms` |
| `append_stage(role, brief, result)` | 生成 `StageRecord`（`stage_id = stage_{n}_{role}`）后调用 `append_agent_result` |
| `to_context_buffer` / `to_messages` / `last_message` / `last_assistant_content` | 读取视图 |
| `recalculate_stats` | 从消息列表重算 `total_tokens` / `message_count` / `turn_count` / `tool_calls_count` |
| `to_summary` | 生成列表用的 `ConversationSummary` |
| `compact_history(keep_recent_messages)` | 见「压缩」 |
| `generate_stats_report` / `format_stats_markdown` | 统计报表（估算 `字符数 / 3` 个 token） |
| `has_user_turns` | 持久化闸门，见下 |

`ConversationFilter`（同文件）字段：`status`、`parent_id`、`keyword`（匹配 title / id / tags，忽略大小写）、`limit`、`offset`、`min_turns`。`matches()` 按 `min_turns → status → parent_id → keyword` 顺序短路。`ConversationFilter::new()` 不过滤任何行。

## 持久化：`ConversationStore`

`src/store/mod.rs` 定义 trait（`#[async_trait]`，`Send + Sync`）：

| 方法 | 契约 |
|---|---|
| `save(&Conversation)` | 新增或覆盖。**没有用户轮的会话是 no-op，返回 `Ok(())`** |
| `load(id) -> Option<Conversation>` | 不存在返回 `None`，不是错误 |
| `delete(id) -> bool` | 不存在返回 `false` |
| `list(&ConversationFilter) -> Vec<ConversationSummary>` | 按 `updated_at_ms` 降序，再应用 `offset` / `limit` |
| `exists(id) -> bool` | 默认实现 = `load(id).is_some()` |

### `MemoryConversationStore`（`src/store/memory.rs`）

`Arc<RwLock<HashMap<String, Conversation>>>`，`Clone` 即共享。额外提供 `count()` 与 `clear()`。`list` 在读取时现算 summary。同样执行无用户轮不落行的规则。

### `FsConversationStore`（`src/store/fs.rs`）

磁盘布局（root 默认 `thunder_subdir("conversations")`，即 `$THUNDER_CONFIG_DIR` 或 `~/.thunder` 下的 `conversations/`）：

```
<root>/
├── index.json                     # HashMap<id, ConversationSummary>
└── <session_id>/
    ├── conversation.json          # 完整 Conversation
    └── raw_transcript.jsonl       # 可选：压缩前的原始消息，每行一个 JSON
```

- `FsConversationStore::new(root)`：`create_dir_all` + 载入或重建索引。
- `default_store_root()`：见上。
- `root()`：暴露根路径。
- `save_raw_transcript(session_id, &[ChatMessage]) -> PathBuf` / `load_raw_transcript(session_id)`：sidecar JSONL，**不是**压缩后的工作历史。写入同样是 tmp + rename，可重复调用（每次传完整快照即可）。
- `rebuild_index()`：扫描所有含 `conversation.json` 的子目录，重写 `index.json`。

索引策略：`index.json` 存在且可解析就加载；损坏或不可读时降级为「空索引」并走 `rebuild_index`，不会让 `list` 直接失败。

## 原子性与并发

写入一律「唯一临时文件 + `rename`」：

- 会话体：`<id>/.conversation.json.tmp.<pid>.<now_ms>`。
- 索引：`<root>/.index.json.tmp.<pid>.<now_ms>`。
- 原始记录：`<id>/.raw_transcript.jsonl.tmp.<pid>.<now_ms>`。

临时文件名带 pid 与毫秒，两个进程写同一 root 时不会互相覆盖对方的半成品。

索引写不是简单的整体覆盖，而是 read-merge-write（`persist_index`）：

1. 重新读取磁盘索引（缺失/损坏 → 空表，不报错）；
2. 用内存行合并，同一 id 取 `updated_at_ms` 更大者（`merge_index_maps`）；
3. 丢弃 `conversation.json` 已不存在的行（`prune_missing_dirs`，stat 不确定时保留）；
4. 原子写入。

`list` 同样合并「内存 + 磁盘」并剪枝，因此 TUI 与 daemon 共用 root 时互相能看见对方新建/删除的会话，无需重启。

重试：`new()` 对「刚创建的 root 上并发 `create_dir_all` / `read_dir` 竞态导致的瞬时 `NotFound`」重试，最多 10 次、间隔 10ms；其他错误立刻返回。回归用例见 `tests/store_concurrency_test.rs`（8 个并发任务 × 100 轮）。

## 持久化闸门

规则：**只有至少一条用户消息的会话才会落盘**。宿主可以先写 system prompt、再挂一条助手问候，但那不是会话。

- 判定：`Conversation::has_user_turns()` 直接遍历 `messages` 找 `ChatMessage::User`，不看 `stats.turn_count`（后者只由 `touch()` 刷新，可能滞后）。
- 两个 store 的 `save` 都在开头短路；`FsConversationStore` 连目录都不会创建。
- 列表面向上，`ConversationFilter::with_min_turns(1)` 会把旧版本写下的空行也过滤掉；TUI 与 catalog 都用它。
- `ConversationManager::create` / `create_with_prompt` / `get_or_create` 返回的是**草稿**，本身不写盘。

用例：`tests/durability_test.rs`。

## 标题

crate 内的部分只有策略与占位符，模型调用不在本 crate：

| 项 | 位置 | 行为 |
|---|---|---|
| `provisional_title(prompt)` | `src/types.rs` | 空白折叠成一行；超过 `PROVISIONAL_TITLE_CHARS = 30` 个字符时截断并追加 `...`；否则原样返回 |
| `Conversation::is_title_placeholder()` | 同上 | `None`、空白、`"new conversation"`、`"untitled"`、或以 `...` 结尾（长度 ≥ 3）时为真 |
| `Conversation::is_title_manual()` | 同上 | `title_source == Some("manual")` |
| `add_user_message` 自动标题 | 同上 | 仅在 `title.is_none()` 时取前 40 字符（不做 `...`，因此不算占位） |

真正的 LLM 命名在 `thunder-agent-providers::naming`（`generate_title` / `clean_generated_title` / `clamp_title` / `fallback_title`，实用模型 60s 超时）；宿主（`thunder-agent-core/tui` 的 `src/title.rs`、`thunder-agent-daemon`）负责读会话、拒绝覆盖 manual 标题（除非 `force`）、重新 `load` 后写回 `title` + `title_source = "auto"`。本 crate 只提供字段与判定。

用例：`tests/title_test.rs`。

## 一次典型流程

```mermaid
flowchart LR
  Draft["Conversation::new(id)
  + with_system_prompt"] --> Turn["add_user_message(text)"]
  Turn -->|has_user_turns() == true| Save["store.save"]
  Save --> Body["tmp + rename
  <id>/conversation.json"]
  Save --> Index["read-merge-write
  index.json"]
  Index --> List["store.list(filter)
  updated_at 降序"]
  Run["AgentRunResult"] --> Append["append_agent_result"]
  Append --> Save
```

`ConversationManager` 的 `create` / `create_with_prompt` / `get_or_create` 只产出上面那个草稿，不写盘；`append_user_message` / `append_run_result` / `append_stage` 则是「load → 变更 → save」的便捷封装，目标不存在时返回 `NotFound`。

## 轮次、压缩、导出、编排

**轮次**（`src/turn.rs`）：`Turn { turn_index, user_message, assistant_messages, tool_messages, final_content, estimated_tokens }`。`extract_turns` 以用户消息开新轮，system 不参与，assistant/tool 归入当前轮（无当前轮时也会开一个），`final_content` 取最后一条非空 assistant 文本。`truncate_turns(conv, keep_last_n)` 保留 system prompt + 最近 N 轮；`keep_last_n == 0` 只留 system prompt。

**压缩**（`Conversation::compact_history`）：固定 `messages[0]` 的 system prompt；若 `messages[1]` 也是 system（loop 写入的 checkpoint 摘要）则保留在摘要之后、通知之前；丢弃的区间用一条 `[Earlier context trimmed: N message(s) removed ...]` 用户消息**如实说明**，不伪造摘要；保留窗口不会以 tool 结果开头（避免孤立 tool result 被 provider 拒绝）。返回 `(压缩前, 压缩后)` 消息数。用例：`tests/compaction_test.rs`。

**导出**（`src/exporter.rs`）：`to_markdown`（标题、ID、状态、统计、stages、按轮次展开的对话，含 tool call/result 代码块）、`to_json` / `from_json`、`to_openai_messages`（原样返回 `messages`）。`ConversationManager::format_session_catalog` 生成 `/resume` 用的 Markdown 目录表，`resume(id_or_index)` 先按 1-based 序号在同样的 `min_turns(1)` 列表里解析，再退回按 ID 查找。

**编排**（`src/orchestration.rs`）：`OrchestrationTopology::{Sequential, Parallel, Delegate}`（`as_str` → `"sequential"` / `"parallel"` / `"delegate"`）。`OrchestrationHelper` 提供 `create_branch`（继承 system prompt、写 `OrchestrationMeta`、标题 `Branch [<topology>]: <role>`）、`record_sequential_stage`、`merge_parallel_into_parent`（合成一条 assistant 消息并为每个分支记 stage）、`record_delegated_task`（写成一条带 `[sub_conversation_id: ...]` 的 tool 消息）。用例：`tests/orchestration_conversation_test.rs`。

**桥接**（`src/bridge.rs`）：`From<&Conversation>` / `From<Conversation>` → `ContextInput::Messages`；`as_context_input()`、`build_context_buffer()`。

## 失败方向

| 场景 | 结果 |
|---|---|
| 无用户轮的 `save` | `Ok(())`，无目录、无索引行 |
| `load` 不存在的 id | `Ok(None)` |
| `delete` 不存在的 id | `Ok(false)` |
| `index.json` 损坏 | 告警 + 重建（`rebuild_index`）；合并路径降级为空表 |
| 两个进程同时写索引 | read-merge-write，新者胜；已删除的行不会被复活 |
| 并发在空 root 上构造 store | 瞬时 `NotFound` 重试 10 × 10ms，之后仍失败才返回 `ConversationError::Io` |
| `ConversationManager::append_*` 目标不存在 | `ConversationError::NotFound(id)` |
| `create` / `fork` id 已存在 | `ConversationError::AlreadyExists(id)` |
| 序列化/IO 失败 | `ConversationError::Serialization` / `ConversationError::Io` |

`ConversationError` 枚举：`NotFound(String)`、`AlreadyExists(String)`、`StoreError(String)`、`Io(io::Error)`、`Serialization(serde_json::Error)`、`InvalidOperation(String)`；实现 `Display` + `Error`（`Io` / `Serialization` 有 `source`），并有 `From<std::io::Error>` / `From<serde_json::Error>`。

## 测试

| 文件 | 覆盖 |
|---|---|
| `tests/conversation_test.rs` | Memory / Fs 的 CRUD、`ContextInput` 桥接、raw sidecar 往返、共享 root 的保存与删除 |
| `tests/durability_test.rs` | 无用户轮不落盘（两个 store）、`min_turns` 过滤、`has_user_turns` |
| `tests/store_concurrency_test.rs` | 空 root 上 8 并发 × 100 轮构造 store |
| `tests/title_test.rs` | 占位标题判定、`provisional_title` 截断/原样/折行 |
| `tests/compaction_test.rs` | system prompt 固定、无孤立 tool result、如实通知、`keep=0`、checkpoint 摘要不丢 |
| `tests/orchestration_conversation_test.rs` | sequential / parallel / delegate 的会话留痕 |

`./test.sh` 等价于 `cargo test`。

## 扩展点

- 新后端：实现 `ConversationStore`（五个方法，`exists` 可继承）。`ConversationManager<S>` 对 store 泛型，无需改动。
- 新元数据：写进 `metadata`（`serde_json::Value`），不新增列就不会破坏旧文件。
- 新导出格式：在 `ConversationExporter` 增加函数，内部复用 `extract_turns`。
- 新压缩策略：`compact_history` 是 `Conversation` 的方法，宿主自行决定何时调用；token 估算统一走 `ContextBuffer::calculate_message_tokens`。
