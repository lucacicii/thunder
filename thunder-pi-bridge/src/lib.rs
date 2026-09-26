//! thunder-pi-bridge: the single LLM transport for Thunder, backed by
//! [`@earendil-works/pi-ai`](https://www.npmjs.com/package/@earendil-works/pi-ai).
//!
//! Architecture:
//!
//! ```text
//! thunder (Rust)                          sidecar (Node)
//! ┌──────────────────────┐   NDJSON/stdio  ┌─────────────────────────┐
//! │ PiAiClient           │ ──────────────► │ bridge.mjs              │
//! │  impl LLMClientTrait │ ◄────────────── │  message/tool mapping   │
//! │ PiAiBridge (process) │                 │  @earendil-works/pi-ai  │
//! └──────────────────────┘                 │   stream() → providers  │
//!                                          └─────────────────────────┘
//! ```
//!
//! All model-dialect knowledge (deepseek/zai/qwen/openrouter/together
//! thinking formats, thinkingLevelMap translation, developer-role handling,
//! usage accounting) lives in pi-ai — Thunder stops duplicating it.
//!
//! Protocol (one JSON object per line, UTF-8):
//!
//! Rust → Node:
//! - `{cmd:"health", id}` — readiness probe (also triggers pi-ai resolution
//!   and, if needed, a one-time `npm install` into the bridge dir)
//! - `{cmd:"stream", id, model:{…BridgeModel…}, messages:[thunder ChatMessage],
//!    tools:[thunder ToolDefinition], thinkingLevel?, temperature?, topP?,
//!    maxTokens?}`
//! - `{cmd:"cancel", id}`
//! - `{cmd:"list_models", id}` — pi-ai builtin catalog
//! - `{cmd:"shutdown"}`
//!
//! Node → Rust:
//! - `{id, type:"ready", version, node}`
//! - `{id, type:"text_delta", delta}`
//! - `{id, type:"reasoning_delta", delta}`
//! - `{id, type:"done", content?, toolCalls:[{id,name,arguments(object)}],
//!    finishReason, usage:{input,output,cacheRead,reasoning?}}`
//! - `{id, type:"error", message}`
//! - `{id, type:"models", models:[…]}`
//! - `{type:"fatal", message}` — bridge-level failure (pi-ai not loadable)

pub mod client;
pub mod model;
pub mod process;

pub use client::{global_bridge, PiAiClient};
pub use model::BridgeModel;
pub use process::{default_bridge_dir, BridgeModelInfo, PiAiBridge};

pub mod prelude {
    pub use crate::client::{global_bridge, PiAiClient};
    pub use crate::model::{
        BridgeModel, API_ANTHROPIC_MESSAGES, API_GOOGLE_GENERATIVE_AI, API_OPENAI_COMPLETIONS,
        API_OPENAI_RESPONSES,
    };
    pub use crate::process::{default_bridge_dir, BridgeModelInfo, PiAiBridge};
}
