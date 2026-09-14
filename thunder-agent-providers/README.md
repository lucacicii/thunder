# Thunder Agent Providers

Rust adapter layer inspired by `@earendil-works/pi-ai`.

This crate does **not** depend on Node or `pi-ai` at runtime. It reuses Pi's
`models.json` / `auth.json` conventions and exposes `LLMClientTrait` implementations
for Thunder Agent Loop.

## Supported APIs (phase 1)

- `openai-completions`
- `anthropic-messages`
- `google-generative-ai`

## Config

- `~/.pi/agent/models.json`
- `~/.pi/agent/auth.json`
- Environment variables: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`
