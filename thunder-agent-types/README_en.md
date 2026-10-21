[English](README_en.md) | [简体中文](README.md)

# thunder-agent-types

## Purpose

Thunder's wire vocabulary: the messages, events, errors, notices, tool declarations and results, policy declarations, scopes and trace records that the agent loop shares with its hosts.

Data and trait declarations only. No I/O, no execution, no judgement — depending on this crate does not drag in the loop or the enforcement layer. A tool pack, a UI or an embedder can depend on it alone.

## Crate boundary

External dependencies: `serde`, `serde_json`, `async-trait` — nothing else.

Downstream crates (those declaring `thunder-agent-types` in `Cargo.toml`): `thunder-agent-loop`, `thunder-agent-policy`, `thunder-agent-runtime`, `thunder-agent-providers`, `thunder-agent-skills`, `thunder-agent-mcp`, `thunder-agent-pack-code`, `thunder-agent-root`, `thunder-agent-daemon`, `thunder-agent-core/tui`.

"Zero internal dependencies" is a hard constraint, not tidiness: both the kernel (`thunder-agent-loop`) and the policy layer (`thunder-agent-policy`) depend on this crate. A dependency back onto either would form a cycle Cargo cannot build. So only nouns both sides need may live here; every "who decides" question belongs upstream.

## Modules

| Module | Contents |
|---|---|
| `message` | `Role`, `ToolCall` / `ToolCallFunction`, `ContentPart`, `ChatMessage` (system / user / assistant / tool) |
| `event` | `AgentEvent`, `ObservedEvent`, `TurnStats`, `AgentStats`, `FinishReason`, `EventDropCounters` |
| `tool` | `ToolDefinition` / `FunctionDefinition`, `ToolExecutionResult`, `ToolEventSink`, `ProgressReporter` |
| `policy` | `ToolEffect`, `Caller`, `describe_call` — what a call *is*, never whether it is allowed |
| `scope` | `Scope`, `ToolScope`, `UnknownScope` |
| `notice` | `SystemNotice` and its Markdown rendering |
| `trace` | `ToolTrace`, `Decision`, `DenyBy`, `GuardHit`, `GuardKind`, `AskAnswer`, `TraceResult`, `TraceTiming`, `ToolTraceSink`, `TRACE_SCHEMA` |
| `error` | `AgentError`: recoverable unit-level failure |

## Core types

### Messages

```rust
ChatMessage::System { content, name? }
ChatMessage::User { content, parts?, name? }
ChatMessage::Assistant { content: Option<String>, tool_calls?, refusal?, name?, reasoning?, stats? }
ChatMessage::Tool { tool_call_id, content, name? }
```

- `Assistant.content` is always serialised (written as `null` when absent) to satisfy the OpenAI/DeepSeek request shape.
- `User.parts` appears only when attachments exist, so text-only messages stay byte-identical to the original wire format.
- `ContentPart::Image { mime_type, data, name?, path?, sha256? }`: `data` is base64 without a `data:` prefix; `path`/`sha256` are the persistence reference written when the conversation store offloads the bytes.
- `ChatMessage::assistant_turn(...)` is the one-message-per-turn persistence shape, carrying that turn's `reasoning` and `TurnStats`.

### Tools

| Type | Meaning |
|---|---|
| `FunctionDefinition { name, description, parameters, strict? }` | The model-visible function declaration; `parameters` is a JSON Schema |
| `ToolDefinition { tool_type, function }` | `tool_type` is always `"function"` |
| `ToolExecutionResult { output, is_error, truncated, original_bytes, duration_ms, telemetry? }` | What a tool hands back; `telemetry` is a model-visible `SystemNotice` |
| `ToolEventSink::emit_custom(kind, payload)` | Business-defined event outlet; `payload` is opaque to the kernel |
| `ProgressReporter::report_progress(...)` | Progress callback for long-running work |

`ToolExecutionResult::success` / `error` both set `original_bytes` to `output.len()`. `with_telemetry` renders the notice as Markdown and appends it to `output` (or becomes the body when the output is empty), refreshing `original_bytes` at the same time.

### Events

`AgentEvent` is encoded with `#[serde(tag = "type", rename_all = "snake_case")]`. Variants:

`TurnStart`, `TokenDelta`, `ReasoningDelta`, `ToolCallChunk`, `ToolCallReady`, `ToolExecStart`, `ToolExecResult`, `ToolTrace`, `TelemetryNotice`, `TurnEnd`, `LoopComplete`, `ContextCompacted`, `SteerAccepted`, `Error`, `Custom`, `GateResult`.

`ObservedEvent` adds host-side routing to an event:

| Field | Meaning |
|---|---|
| `agent_id` | The emitting unit, so a scheduler can demux many agents |
| `event` | The `AgentEvent` |
| `timestamp_ms` | Wall-clock time the loop emitted it; `#[serde(default)]`, so old traces deserialize as `0` |

The `ToolTrace` variant is boxed (`Box<ToolTrace>`): a trace dwarfs every other variant, and events are cloned once per subscriber.

### Policy and scope

`ToolEffect` classifies by tool name; an unregistered name falls through to `Other` (which behaves like `Write`) — an unrecognised tool is never assumed harmless.

| Effect | Classification (`ToolEffect::of`) |
|---|---|
| `read` | `read_file` `grep` `find` `ls` `list_dir` `read` `glob` `exit_plan_mode` |
| `write` | `write_file` `edit` `write` `apply_patch` `notebook_edit` |
| `exec` | `bash` `shell` `powershell`, plus any name containing `exec` / `shell` / `terminal` / `powershell` / `command` or ending in `_bash` |
| `plan_artifact` | `write_plan` |
| `other` | Everything else |

The serialised spellings (`read` / `write` / `exec` / `plan_artifact` / `other`) are a wire contract shared by traces and remote tool declarations.

`Caller` (internally tagged `caller`) separates `Model` from `Plugin(String)`: an approval dialog must be able to say a plugin is asking, otherwise "the user agreed" gets mistaken for agreeing with the assistant.

`Scope` is a *location*, never a verdict:

| Variant | Meaning |
|---|---|
| `InScope` | Inside the host's declared working area |
| `OutOfScope` | Outside it; not a refusal by itself, it just earns no automatic allowance |
| `Critical` | Off limits (a `.git` directory, a credential store): the kernel refuses unconditionally, in every mode, without asking |
| `Unknown` (default) | The host cannot tell: no automatic allowance, so the call is asked about |

`ToolScope::scope_of(tool, effect, arguments) -> Scope` is implemented by the host; it is synchronous and must be total — an inability to decide returns `Unknown`, never an error. `UnknownScope` is the implementation that knows nothing, for tests and hosts with no working area.

The asymmetry is the point: scope may only *relax* a call (an in-scope write under `acceptEdits` runs without a prompt), never turn a call the gate would allow into a refusal. The one exception is `Critical`, where the host states that asking would be wrong.

`describe_call(args)` is the single source of approval wording: `command` first, then `path`, otherwise compact JSON — each cut to 200 characters.

### Notices, traces and errors

- `SystemNotice { layer, action, ground_truth, self_healed?, guidance? }` is the *model-facing* half of telemetry: what the system did on the model's behalf and what the world actually looks like. The human-facing half is `ToolTrace`. They are deliberately separate: the model needs ground truth to keep working; a durable audit artifact needs redaction and structure.
- `ToolTrace` is the *human/persistence-facing* record of one call (fields in `thunder-agent-types/src/trace.rs`); `TRACE_SCHEMA = 1` and readers should dispatch on `schema`. `Decision` distinguishes `Allow` / `AllowByRule` / `Asked` / `Deny { reason, by }` / `Cancelled`; `DenyBy` distinguishes the kernel's `Guard` from the host's `Gate`.
- `ToolTraceSink::record(&ToolTrace)` is one-directional: the kernel hands over a record and forgets it, and what arrives is already redacted.
- `AgentError` covers recoverable unit-level failures only (`AlreadyRunning`, `Terminated`, `Config`, `Other`). Cancellation, budget and max-turns are terminal outcomes and live in `FinishReason`, not in an error.

## Event contract: `approx_bytes` and `EventDropCounters`

The event queue has a byte budget (`AgentConfig::max_event_bytes` on the loop side, 8 MiB by default), and each enqueue is charged `ObservedEvent::approx_bytes()`.

`approx_bytes` is an estimate, not a serialisation:

```
96 (fixed frame: discriminator, timestamp, etc.) + agent_id.len() + payload
```

`payload` counts only the fields that can actually grow: text deltas use `len()`; `ToolCallChunk` uses id/name/arguments_delta lengths; `ToolCallReady` uses id/name/arguments; `ToolExecStart` uses name + `arguments.to_string().len()`; `ToolExecResult` uses `result.output.len()`; `ToolTrace` uses the `serde_json::to_string` length; `TelemetryNotice` sums its strings; `LoopComplete` uses `final_content`; `SteerAccepted` uses `message`; `Error` uses `message`; `Custom` uses kind + payload; `GateResult` uses verdict. Unlisted variants count 0 payload and pay only the fixed frame.

Running `serde_json` on every token delta would cost more than the bound saves, hence the estimate.

`EventDropCounters` (`AtomicU64`, `Relaxed`) turns delivery loss into an observable fact, in two classes:

| Counter | Trigger | Meaning |
|---|---|---|
| `micro_deltas` | `TokenDelta` / `ReasoningDelta` / `ToolCallChunk` dropped by `try_send` when the channel is full | Streaming deltas missing; the final message can still reconstruct the content |
| `structural` | A lifecycle or business event (including `Custom`) still undelivered after a bounded wait, or a single event larger than the whole budget (`TurnEnd`, `LoopComplete`, traces) | The run's record may be incomplete |

`EventDropCounters::shared()` shares one counter set between an emitter, its sinks and the host; `total()` sums both. Hosts should treat `structural() > 0` as "the run's record may be incomplete", not as an ignorable metric.

## Defaults and wire constants

| Item | Value |
|---|---|
| `ObservedEvent::approx_bytes` fixed frame `FRAME` | 96 B |
| `TRACE_SCHEMA` | 1 |
| `describe_call` truncation | 200 chars |
| `AgentEvent` discriminator | `"type"`, `snake_case` |
| `ObservedEvent.timestamp_ms` default | 0 |
| `Scope` default | `unknown` |
| `ToolEffect` for an unknown name | `other` |
| `FinishReason` spellings | `done` / `max_turns_exceeded` / `budget_exceeded` / `cancelled` / `error` |

## Failure direction

- This crate produces no runtime failures, only data. Its deserialisation is deliberately lenient: `timestamp_ms` and every added optional field carry `#[serde(default)]`, so old traces still load.
- Defaults always pick the safe side: `Scope::Unknown` earns no automatic allowance; an unclassified tool effect is `Other` (treated as `Write`); any caller that is not the model must be labelled explicitly.
- `AgentError` never carries terminal outcomes; treating cancellation or a budget stop as an error would push callers into a retry loop.

## Tests

10 inline unit tests, all about the wire format rather than behaviour:

- `event.rs`: an old event without `timestamp_ms` deserializes as 0; the `tool_trace` variant round-trips under its own tag.
- `message.rs`: a plain user message emits no `parts` key; multimodal round-trips keep `mimeType`/`data` and omit empty `path`/`sha256`; empty `parts` collapse to a plain message.
- `notice.rs`: `format_markdown` field order and optional sections.
- `policy.rs`: unknown tools are not assumed harmless; the wire spellings of `read`/`exec`/`plan_artifact`.
- `scope.rs`: `Scope::default()` is `Unknown` and `UnknownScope` always answers `Unknown`; the `in_scope` / `out_of_scope` spellings.

## Extension points

- Implement `ToolEventSink` to let tools emit host-defined events (carried as `AgentEvent::Custom`).
- Implement `ProgressReporter` to feed a progress UI for long-running work.
- Implement `ToolScope` to tell the kernel where a call lands.
- Implement `ToolTraceSink` to persist traces into your own store.
- Add your own events via `AgentEvent::Custom { kind, payload }`; the kernel never interprets `kind`, and unknown kinds are safe for a host to ignore.
