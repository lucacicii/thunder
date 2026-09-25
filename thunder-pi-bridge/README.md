# thunder-pi-bridge

Thunder's single LLM transport, proxying model execution through [`@earendil-works/pi-ai`](https://www.npmjs.com/package/@earendil-works/pi-ai) via an asynchronous Node.js sidecar.

## Architecture

```text
Thunder (Rust)                             Node Sidecar (ESM)
┌──────────────────────┐   NDJSON / stdio   ┌─────────────────────────┐
│ PiAiClient           │ ─────────────────► │ runner/bridge.mjs       │
│  impl LLMClientTrait │ ◄───────────────── │  message/tool mapping   │
│ PiAiBridge (process) │                    │  @earendil-works/pi-ai  │
└──────────────────────┘                    │   stream() → providers  │
                                            └─────────────────────────┘
```

## Why pi-ai?

- **Zero-maintenance dialect matrix**: DeepSeek, OpenAI o1/o3, Qwen, Moonshot, zAI, Together, OpenRouter, Anthropic, Google, and Ollama dialects are maintained in `@earendil-works/pi-ai`.
- **Thinking Level Map translation**: Preserves provider-specific effort level mappings (e.g. `high` → `max`, `off` → `{ type: "disabled" }`).
- **Unified token accounting**: Accurately maps input tokens, output tokens, reasoning tokens, and cache reads.
- **Auto-healing & resilience**: Bridge process auto-restarts on crash, supports cancellation signals, and handles chunk idle timeouts.

## Protocol (NDJSON over stdio)

### Rust → Node
- `{"cmd":"health","id":"..."}`
- `{"cmd":"stream","id":"...","model":{...},"messages":[...],"tools":[...],"thinkingLevel":"..."}`
- `{"cmd":"cancel","id":"..."}`
- `{"cmd":"list_models","id":"..."}`
- `{"cmd":"shutdown"}`

### Node → Rust
- `{"id":"...","type":"ready","version":"...","node":"..."}`
- `{"id":"...","type":"text_delta","delta":"..."}`
- `{"id":"...","type":"reasoning_delta","delta":"..."}`
- `{"id":"...","type":"done","content":"...","toolCalls":[...],"finishReason":"...","usage":{...}}`
- `{"id":"...","type":"error","message":"..."}`
- `{"id":"...","type":"models","models":[...]}`
