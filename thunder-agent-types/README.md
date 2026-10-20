# thunder-agent-types

The wire vocabulary the loop and every host agree on.

Pure data and traits, no I/O, no policy:

- `message` — `ChatMessage`, `Role`, `ContentPart`, `ToolCall`
- `event` — `AgentEvent`, `ObservedEvent`, `FinishReason`, stats, drop counters
- `tool` — `ToolDefinition`, `ToolExecutionResult`, `ToolEventSink`, `ProgressReporter`
- `notice` — `SystemNotice`, the model-facing half of telemetry
- `error` — `AgentError`

A crate that only needs to *describe* a call (a tool pack, a UI) depends on this
instead of the loop. Policy types (`ToolEffect`, `ToolGate`, `Scope`, `UiRequest`)
deliberately live elsewhere.
