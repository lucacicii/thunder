# Thunder Agent Providers

Model catalog and configuration management layer compatible with `@earendil-works/pi-ai`.

This crate loads provider and model specifications from `models.json` and `auth.json`,
handling model discovery, context window caching, and thinking level mappings.
LLM execution transport is delegated directly to `@earendil-works/pi-ai` via `thunder-pi-bridge`.

## Supported Provider APIs (via `@earendil-works/pi-ai`)

- `openai-completions` (OpenAI, DeepSeek, OpenRouter, Qwen, Moonshot, zAI, Groq, Ollama, etc.)
- `anthropic-messages` (Anthropic Claude 3.5 / 3.7)
- `google-generative-ai` (Google Gemini)

## Config

- `~/.pi/agent/models.json`
- `~/.pi/agent/auth.json`
- Environment variables: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`
