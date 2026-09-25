# ⚡ Thunder Agent Providers

> **LLM Model Catalog & Configuration Layer — Fully Compatible with `@earendil-works/pi-ai`**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-providers` loads and parses `models.json` (model catalog) and `auth.json` (credentials), providing model discovery, context window caching, availability resolution, and thinking level mappings. All LLM execution transport is delegated to [`thunder-pi-bridge`](../thunder-pi-bridge) (a Node.js sidecar running `@earendil-works/pi-ai`); this crate **holds no custom dialect adapters**.

---

## 🚀 Core Responsibilities

1. **Model Catalog Discovery & Parsing**:
   - Loads `~/.pi/agent/models.json` and project-level `models.json`.
   - Caches model specification metadata: context window size, max output tokens, and capability flags (tools / reasoning).
2. **Credential & Availability Resolution**:
   - Parses `~/.pi/agent/auth.json` and environment variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`).
   - Annotates each model's `available` status for upstream routing and UI decisions.
3. **Thinking Level Mapping**:
   - Aligns generic `thinking_level` semantics to provider-specific effort tiers.
4. **Pure Catalog Layer (Transport-Free)**:
   - Contains no HTTP client or transport implementation. Model invocations are performed exclusively through `PiAiClient` (`thunder-pi-bridge`).

---

## 📡 Supported Provider APIs (via `@earendil-works/pi-ai`)

| API Protocol | Covered Providers |
| :--- | :--- |
| `openai-completions` | OpenAI, DeepSeek, OpenRouter, Qwen, Moonshot, zAI, Groq, Ollama, etc. |
| `anthropic-messages` | Anthropic Claude 3.5 / 3.7 |
| `google-generative-ai` | Google Gemini |

---

## 🛠️ Testing

```bash
# Run catalog parsing & metadata cache tests
cargo test -p thunder-agent-providers
```
