[English](README_en.md) | [简体中文](README.md)

# thunder-conversation

The session model and its persistence layer: it defines a conversation as serializable data, and encodes "what counts as a session" inside the store.

## Role

`thunder-agent-loop` only knows a single run (`ContextInput` / `AgentRunResult` / `ChatMessage`); it does not own anything that outlives one. This crate supplies that layer:

- `Conversation`: message list + metadata + stats — the entity that lands on disk.
- `ConversationStore`: the read/write trait, with `MemoryConversationStore` and `FsConversationStore` implementations.
- `ConversationManager`: CRUD / fork / archive / resume on top of a store.
- A turn view: `extract_turns` / `truncate_turns`, so UI "turn N" and "keep the last N turns" share one definition.
- `OrchestrationMeta` / `StageRecord`: the multi-agent audit trail.
- `ConversationExporter`: Markdown / JSON / OpenAI-message exports.

## Crate boundary

Internal dependencies (`Cargo.toml`):

| Dependency | Purpose |
|---|---|
| `thunder-agent-loop` | `ChatMessage` / `Role` / `ToolCall` / `ContentPart`, `ContextBuffer` (token estimation), `AgentRunResult`, `paths::thunder_subdir` |

Beyond the standard library it uses only `tokio`, `serde`/`serde_json`, `async-trait`, `tracing`. It does not depend on `thunder-agent-policy` / `thunder-agent-root`: this crate knows nothing about tools, models or hosts.

Workspace members that depend on it:

| Member | How |
|---|---|
| `thunder-agent-root` | Optional dependency behind feature `conversation` (on by default); used by `plugins/conversation.rs` and `plugins/standard.rs` via `ConversationStore` |
| `thunder-agent-daemon` | Direct dependency; `service.rs` owns the session |
| `thunder-agent-core/tui` | Direct dependency; persistence and title generation for `App` |

## Core types

`Conversation` (`src/types.rs`):

| Field | Type | Meaning |
|---|---|---|
| `id` | `String` | Session id, also the directory name on disk |
| `title` | `Option<String>` | Display name; a placeholder may be replaced |
| `title_source` | `Option<String>` | `"auto"` (model-generated) or `"manual"` (user-set) |
| `parent_id` | `Option<String>` | Fork / branch origin |
| `system_prompt` | `Option<String>` | Kept consistent with the system message at `messages[0]` |
| `model` / `workspace` / `thinking_level` | `Option<String>` | Run bindings that survive across runs |
| `shared_roots` | `Vec<String>` | Extra roots with the workspace's standing |
| `status` | `ConversationStatus` | `Active` (default) / `Archived` / `Deleted` / `Paused` |
| `messages` | `Vec<ChatMessage>` | The authoritative message sequence |
| `stages` | `Vec<StageRecord>` | Orchestration stage records |
| `orchestration` | `Option<OrchestrationMeta>` | Topology, role, run id, routing reason |
| `metadata` | `HashMap<String, serde_json::Value>` | Free-form; a `tags` array is read by `to_summary` |
| `stats` | `ConversationStats` | See below |
| `created_at_ms` / `updated_at_ms` | `u64` | Millisecond timestamps (`now_ms()`) |

`ConversationStats` carries two token figures with different meanings; they are not interchangeable:

- `total_tokens`: estimate of the *current working context*, recomputed from the message list by `ContextBuffer::calculate_message_tokens` on every `touch()`.
- `total_used_tokens`: cumulative provider-reported usage (including cache reads/writes), never recomputed from text — hosts accumulate it.

Remaining fields: `message_count`, `turn_count` (number of user messages), `tool_calls_count`, `duration_ms`.

Constructors and mutators (same file):

| Method | Behaviour |
|---|---|
| `new(id)` | `Active`, empty messages, `created_at_ms = updated_at_ms = now` |
| `with_title` / `with_parent_id` / `with_model` / `with_workspace` / `with_thinking_level` / `with_orchestration` | Build-time setters |
| `with_system_prompt` | Writes or replaces the system message at `messages[0]`, then recalculates stats |
| `bind_workspace_if_empty` | Binds a workspace only when the field is empty |
| `add_user_message` / `add_user_message_with_parts` | Appends a user turn; if `title` is `None`, the first 40 characters of the content become the title |
| `add_assistant_message` / `add_assistant_turn` / `add_tool_message` / `add_message` | Append and `touch()` |
| `append_agent_result` | Merges `AgentRunResult.messages` (skipping its leading system message when one already exists) and adds `duration_ms` |
| `append_stage(role, brief, result)` | Builds a `StageRecord` (`stage_id = stage_{n}_{role}`), then calls `append_agent_result` |
| `to_context_buffer` / `to_messages` / `last_message` / `last_assistant_content` | Read views |
| `recalculate_stats` | Recomputes `total_tokens` / `message_count` / `turn_count` / `tool_calls_count` from the message list |
| `to_summary` | Produces the `ConversationSummary` used for listings |
| `compact_history(keep_recent_messages)` | See "Compaction" |
| `generate_stats_report` / `format_stats_markdown` | Stats report (tokens estimated as `chars / 3`) |
| `has_user_turns` | The durability gate, see below |

`ConversationFilter` (same file) fields: `status`, `parent_id`, `keyword` (matches title / id / tags, case-insensitive), `limit`, `offset`, `min_turns`. `matches()` short-circuits in the order `min_turns → status → parent_id → keyword`. `ConversationFilter::new()` filters nothing.

## Persistence: `ConversationStore`

`src/store/mod.rs` defines the trait (`#[async_trait]`, `Send + Sync`):

| Method | Contract |
|---|---|
| `save(&Conversation)` | Insert or overwrite. **A conversation with no user turn is a no-op returning `Ok(())`** |
| `load(id) -> Option<Conversation>` | A missing id is `None`, not an error |
| `delete(id) -> bool` | `false` when it did not exist |
| `list(&ConversationFilter) -> Vec<ConversationSummary>` | Descending by `updated_at_ms`, then `offset` / `limit` |
| `exists(id) -> bool` | Default = `load(id).is_some()` |

### `MemoryConversationStore` (`src/store/memory.rs`)

`Arc<RwLock<HashMap<String, Conversation>>>`; cloning shares it. Extra helpers: `count()` and `clear()`. `list` computes summaries on read. The no-user-turn rule is enforced here too.

### `FsConversationStore` (`src/store/fs.rs`)

On-disk layout (root defaults to `thunder_subdir("conversations")`, i.e. `conversations/` under `$THUNDER_CONFIG_DIR` or `~/.thunder`):

```
<root>/
├── index.json                     # HashMap<id, ConversationSummary>
└── <session_id>/
    ├── conversation.json          # full Conversation
    └── raw_transcript.jsonl       # optional: pre-compaction messages, one JSON per line
```

- `FsConversationStore::new(root)`: `create_dir_all` plus index load-or-rebuild.
- `default_store_root()`: as above.
- `root()`: exposes the root path.
- `save_raw_transcript(session_id, &[ChatMessage]) -> PathBuf` / `load_raw_transcript(session_id)`: the JSONL sidecar, explicitly **not** the working (possibly compacted) history. Also tmp + rename, and idempotent — pass the full snapshot each time.
- `rebuild_index()`: scans every subdirectory holding a `conversation.json` and rewrites `index.json`.

Index policy: a present and parseable `index.json` is loaded; a corrupt or unreadable one degrades to an empty index and goes through `rebuild_index`, so `list` never fails outright.

## Atomicity and concurrency

Every write is "unique temp file + `rename`":

- Conversation body: `<id>/.conversation.json.tmp.<pid>.<now_ms>`.
- Index: `<root>/.index.json.tmp.<pid>.<now_ms>`.
- Raw transcript: `<id>/.raw_transcript.jsonl.tmp.<pid>.<now_ms>`.

Temp names carry pid and milliseconds, so two processes writing the same root never clobber each other's half-written file.

The index write is a read-merge-write (`persist_index`), not a blind overwrite:

1. Re-read the on-disk index (missing/corrupt → empty map, no error);
2. Merge our rows in, newer `updated_at_ms` winning per id (`merge_index_maps`);
3. Drop rows whose `conversation.json` has vanished (`prune_missing_dirs`; an inconclusive stat keeps the row);
4. Write atomically.

`list` merges "memory + disk" and prunes the same way, so a TUI and a daemon sharing a root see each other's new and deleted sessions without a restart.

Retry: `new()` retries the transient `NotFound` produced when several callers build a store for the same *fresh* root at once — at most 10 attempts, 10 ms apart; any other error returns immediately. Regression test: `tests/store_concurrency_test.rs` (8 concurrent tasks × 100 iterations).

## The durability gate

Rule: **only a conversation carrying at least one user message is persisted.** A host may seed a system prompt and attach an assistant greeting first, but that is not a session.

- Test: `Conversation::has_user_turns()` scans `messages` for `ChatMessage::User`; it does not read `stats.turn_count`, which only `touch()` refreshes and can lag a mutation.
- Both stores' `save` short-circuit at the top; `FsConversationStore` does not even create the directory.
- Listing surfaces: `ConversationFilter::with_min_turns(1)` also hides empty rows written by older builds; the TUI and the session catalog both use it.
- `ConversationManager::create` / `create_with_prompt` / `get_or_create` return **drafts** and write nothing.

Tests: `tests/durability_test.rs`.

## Titles

This crate only owns the policy and the placeholder; the model call lives elsewhere:

| Item | Location | Behaviour |
|---|---|---|
| `provisional_title(prompt)` | `src/types.rs` | Collapses whitespace onto one line; beyond `PROVISIONAL_TITLE_CHARS = 30` chars it truncates and appends `...`, otherwise returns the prompt verbatim |
| `Conversation::is_title_placeholder()` | same | True for `None`, blank, `"new conversation"`, `"untitled"`, or a value ending in `...` (length ≥ 3) |
| `Conversation::is_title_manual()` | same | `title_source == Some("manual")` |
| auto-title in `add_user_message` | same | Only when `title.is_none()`: first 40 chars, no ellipsis, so it is not a placeholder |

Actual LLM naming lives in `thunder-agent-providers::naming` (`generate_title` / `clean_generated_title` / `clamp_title` / `fallback_title`, utility model, 60 s timeout). The host (`src/title.rs` in `thunder-agent-core/tui`, and `thunder-agent-daemon`) loads the conversation, refuses to overwrite a manual title unless `force` is set, re-`load`s to avoid clobbering concurrent writes, and writes back `title` + `title_source = "auto"`. This crate only supplies the fields and the predicates.

Tests: `tests/title_test.rs`.

## A typical flow

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
  descending updated_at"]
  Run["AgentRunResult"] --> Append["append_agent_result"]
  Append --> Save
```

`ConversationManager`'s `create` / `create_with_prompt` / `get_or_create` only produce that draft and write nothing; `append_user_message` / `append_run_result` / `append_stage` are "load → mutate → save" conveniences that return `NotFound` when the target does not exist.

## Turns, compaction, export, orchestration

**Turns** (`src/turn.rs`): `Turn { turn_index, user_message, assistant_messages, tool_messages, final_content, estimated_tokens }`. `extract_turns` opens a new turn on a user message, ignores system messages, and files assistant/tool messages under the current turn (opening one if none exists); `final_content` is the last non-empty assistant text. `truncate_turns(conv, keep_last_n)` keeps the system prompt plus the last N turns; `keep_last_n == 0` leaves only the system prompt.

**Compaction** (`Conversation::compact_history`): pins the system prompt at `messages[0]`; if `messages[1]` is also a system message (a checkpoint summary written by the loop) it survives, ahead of the notice; removed context is replaced by an explicit `[Earlier context trimmed: N message(s) removed ...]` user message rather than a fabricated summary; the kept window never starts on a tool result (a provider rejects an orphaned result). Returns `(before, after)` message counts. Tests: `tests/compaction_test.rs`.

**Export** (`src/exporter.rs`): `to_markdown` (title, id, status, stats, stages, per-turn dialogue with tool call/result code blocks), `to_json` / `from_json`, `to_openai_messages` (returns `messages` unchanged). `ConversationManager::format_session_catalog` renders the Markdown table used by `/resume`, and `resume(id_or_index)` resolves a 1-based index against the same `min_turns(1)` listing before falling back to an id lookup.

**Orchestration** (`src/orchestration.rs`): `OrchestrationTopology::{Sequential, Parallel, Delegate}` (`as_str` → `"sequential"` / `"parallel"` / `"delegate"`). `OrchestrationHelper` offers `create_branch` (inherits the system prompt, writes `OrchestrationMeta`, titles the branch `Branch [<topology>]: <role>`), `record_sequential_stage`, `merge_parallel_into_parent` (synthesizes one assistant message and records a stage per branch) and `record_delegated_task` (records a tool message carrying `[sub_conversation_id: ...]`). Tests: `tests/orchestration_conversation_test.rs`.

**Bridge** (`src/bridge.rs`): `From<&Conversation>` / `From<Conversation>` → `ContextInput::Messages`; `as_context_input()`, `build_context_buffer()`.

## Failure directions

| Scenario | Result |
|---|---|
| `save` without a user turn | `Ok(())`; no directory, no index row |
| `load` of an unknown id | `Ok(None)` |
| `delete` of an unknown id | `Ok(false)` |
| Corrupt `index.json` | Warning + `rebuild_index`; the merge path degrades to an empty map |
| Two processes writing the index | read-merge-write, newest row wins; deleted rows are not resurrected |
| Concurrent store construction on an empty root | transient `NotFound` retried 10 × 10 ms, then `ConversationError::Io` |
| `ConversationManager::append_*` on a missing id | `ConversationError::NotFound(id)` |
| `create` / `fork` with an existing id | `ConversationError::AlreadyExists(id)` |
| Serialization / IO failure | `ConversationError::Serialization` / `ConversationError::Io` |

`ConversationError` variants: `NotFound(String)`, `AlreadyExists(String)`, `StoreError(String)`, `Io(io::Error)`, `Serialization(serde_json::Error)`, `InvalidOperation(String)`; it implements `Display` + `Error` (with `source` for `Io` / `Serialization`) and `From<std::io::Error>` / `From<serde_json::Error>`.

## Tests

| File | Coverage |
|---|---|
| `tests/conversation_test.rs` | Memory / Fs CRUD, `ContextInput` bridge, raw sidecar round-trip, shared-root save and delete |
| `tests/durability_test.rs` | No-user-turn conversations are never persisted (both stores), `min_turns` filtering, `has_user_turns` |
| `tests/store_concurrency_test.rs` | 8 concurrent × 100 rounds of store construction on a clean root |
| `tests/title_test.rs` | Placeholder detection, `provisional_title` truncation / verbatim / flattening |
| `tests/compaction_test.rs` | System prompt pinned, no orphaned tool result, honest notice, `keep=0`, checkpoint summary preserved |
| `tests/orchestration_conversation_test.rs` | Sequential / parallel / delegate records |

`./test.sh` is equivalent to `cargo test`.

## Extension points

- New backend: implement `ConversationStore` (five methods; `exists` can be inherited). `ConversationManager<S>` is generic over the store, so nothing else changes.
- New metadata: write into `metadata` (`serde_json::Value`); adding no columns keeps old files loadable.
- New export format: add a function to `ConversationExporter` and reuse `extract_turns`.
- New compaction policy: `compact_history` is a `Conversation` method and the host decides when to call it; token estimation goes through `ContextBuffer::calculate_message_tokens`.
