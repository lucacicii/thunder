# thunder-pi-bridge

> **Thunder's Single LLM Transport — Proxying Model Execution Through [`@earendil-works/pi-ai`](https://www.npmjs.com/package/@earendil-works/pi-ai) via a Node.js Sidecar**

[English](README_en.md) | [简体中文](README.md)

`thunder-pi-bridge` is the **sole LLM transport** in the Thunder ecosystem. It proxies all model invocations through `@earendil-works/pi-ai` running in a Node.js sidecar, eliminating provider dialects, thinking level mappings, and token accounting discrepancies at zero maintenance cost.

---

## 🚀 Architecture

```text
Thunder (Rust)                             Node Sidecar (ESM)
┌──────────────────────┐   NDJSON / stdio   ┌─────────────────────────┐
│ PiAiClient           │ ─────────────────► │ runner/bridge.mjs       │
│  impl LLMClientTrait │ ◄───────────────── │  message/tool mapping   │
│ PiAiBridge (process) │                    │  @earendil-works/pi-ai  │
└──────────────────────┘                    │   stream() → providers  │
                                            └─────────────────────────┘
```

- **`PiAiClient`**: Implements the pure `LLMClientTrait` contract from `thunder-agent-loop` with no HTTP or dialect adaptation code.
- **`PiAiBridge`**: Manages lazy sidecar process startup, lifecycle supervision, automatic crash restart, and graceful shutdown.
- **`GLOBAL` Singleton**: Shares a single sidecar process across all sessions and agent units, amortizing cold-start costs.

---

## ✨ Key Features

- **Pre-bundled & Zero Install**: `runner/bridge.mjs` is pre-bundled with `@earendil-works/pi-ai` and all dependencies via esbuild. Fresh checkouts require **no `npm install`** — only Node.js 20+ installed locally.
- **Custom Override**: Point `$THUNDER_PI_AI_PATH` to a local pi-ai build for development overrides.
- **Zero-Maintenance Dialect Matrix**: DeepSeek, OpenAI o1/o3, Qwen, Moonshot, zAI, Together, OpenRouter, Anthropic, Google, and Ollama dialects are fully absorbed by `@earendil-works/pi-ai`.
- **Thinking Level Mapping**: Translates provider-specific reasoning effort semantics (e.g. `high` → `max`, `off` → `{ type: "disabled" }`), preventing accidental deep-reasoning latency spikes.
- **Unified Token Accounting**: Accurately maps input tokens, output tokens, reasoning tokens, and cache reads.
- **Auto-Healing & Resilience**: Bridge process auto-restarts on crash, supports cancellation signal propagation, and enforces chunk idle timeout watchdogs.

---

## 📡 Protocol (NDJSON over stdio)

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

---

## 🛠️ Testing

```bash
# Run bridge protocol & process lifecycle integration tests
cargo test -p thunder-pi-bridge
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
