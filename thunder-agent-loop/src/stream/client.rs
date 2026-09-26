use crate::types::message::{ChatMessage, ToolCall};
use crate::types::tool::ToolDefinition;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LLMStreamChunk {
    /// User-visible answer content
    Token(String),
    /// Reasoning / chain-of-thought tokens (kept separate from answer content)
    ReasoningToken(String),
    ToolCallChunk(StreamToolCallDelta),
    Completed {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
        finish_reason: String,
        prompt_tokens: Option<usize>,
        completion_tokens: Option<usize>,
        cached_tokens: Option<usize>,
        reasoning_tokens: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub struct ChatRequestOptions {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<usize>,
    pub thinking_level: Option<String>,
    /// Prompt-cache write hint for transports that support it (pi-ai: `none`
    /// disables cache writes). One-off requests (e.g. checkpoint summarization)
    /// should pass `Some("none")` — they will never be reused, so paying the
    /// cache-write premium and polluting the cache is pure waste.
    pub cache_retention: Option<String>,
}

#[async_trait]
pub trait LLMClientTrait: Send + Sync {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String>;
}

/// A default unconfigured client that returns an informative error if invoked.
/// Production LLM transport is provided by `thunder-pi-bridge` (via `@earendil-works/pi-ai`).
#[derive(Debug, Clone, Default)]
pub struct UnconfiguredLLMClient;

#[async_trait]
impl LLMClientTrait for UnconfiguredLLMClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        Err("No LLM client configured. In production, thunder-pi-bridge provides the LLM transport via @earendil-works/pi-ai.".to_string())
    }
}
