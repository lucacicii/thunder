# ⚡ Thunder Agent Providers

> **LLM Model Catalog & Configuration Layer — Fully Compatible with `@earendil-works/pi-ai`**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-providers` loads and parses `models.json` (model catalog) and `auth.json` (credentials), providing model discovery, context window caching, availability resolution, and thinking level mappings. All LLM execution transport is delegated to [`thunder-pi-bridge`](../thunder-pi-bridge) (a Node.js sidecar running `@earendil-works/pi-ai`); this crate **holds no custom dialect adapters**.

---

## 🚀 Core Responsibilities

1. **Model Catalog Discovery & Parsing**:
   - Loads the user-level `~/.thunder/models.json` (or `$THUNDER_CONFIG_DIR/models.json`) and the project-level `<workspace>/.thunder/models.json`.
   - Caches model specification metadata: context window size, max output tokens, and capability flags (tools / reasoning).
2. **Credential & Availability Resolution**:
   - Parses `~/.thunder/auth.json` and environment variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`).
   - Annotates each model's `available` status for upstream routing and UI decisions.
3. **Thinking Level Mapping**:
   - Aligns generic `thinking_level` semantics to provider-specific effort tiers.
4. **Active Thinking-Level Probing**:
   - For reasoning-candidate models in the local catalog that have not yet been probed (`thinking_levels_probed != true`), issues a single lightweight request through an internal HTTP client to infer the supported thinking levels from the provider's error/enum response (see `src/probe.rs`).
   - Results are written back into `models.json` and flagged as probed, so subsequent loads skip the network entirely.

> ⚠️ Transport responsibility: this crate **does not** own model-invocation transport — all LLM execution goes through `thunder-pi-bridge`. It is *not* "zero-HTTP" though: `src/probe.rs` holds a `reqwest` client used solely for the one-time thinking-level probe above. Actual inference traffic never flows through it.

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
