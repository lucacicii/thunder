use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use thunder_protocol::{
    ChatMessage, ChatRequestOptions, LLMClientTrait, LLMStreamChunk, ToolCall, ToolDefinition,
    ToolDescriptor,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// High-level tool call request extracted from a model turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCallRequest {
    pub fn to_wire_tool_call(&self) -> ToolCall {
        ToolCall::new_function(
            self.id.clone(),
            self.name.clone(),
            serde_json::to_string(&self.arguments).unwrap_or_else(|_| "{}".to_string()),
        )
    }

    pub fn from_wire_tool_call(tc: &ToolCall) -> Self {
        let arguments = serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));
        Self {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments,
        }
    }
}

/// Model response for one inference turn.
#[derive(Clone, Debug)]
pub struct LlmTurnResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallRequest>,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cached_tokens: usize,
}

impl LlmTurnResponse {
    pub fn simple(content: impl Into<String>, tool_calls: Vec<ToolCallRequest>) -> Self {
        Self {
            content: content.into(),
            reasoning: None,
            tool_calls,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
        }
    }
}

/// Abstract LLM driver contract for `thunder-core` (§1).
#[async_trait]
pub trait LlmDriver: Send + Sync {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDescriptor],
    ) -> Result<LlmTurnResponse, String>;

    /// Complete one turn while streaming token/reasoning deltas to `event_sink`.
    async fn complete_streaming(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDescriptor],
        turn: usize,
        event_sink: Option<&Arc<dyn thunder_protocol::EventSink>>,
        cancel: &CancellationToken,
    ) -> Result<LlmTurnResponse, String> {
        let _ = cancel;
        let resp = self.complete(messages, tools).await?;
        if let Some(sink) = event_sink {
            if let Some(ref r) = resp.reasoning {
                if !r.is_empty() {
                    sink.on_agent_event(thunder_protocol::AgentEvent::ReasoningDelta {
                        turn,
                        delta: r.clone(),
                    });
                }
            }
            if !resp.content.is_empty() {
                sink.on_agent_event(thunder_protocol::AgentEvent::TokenDelta {
                    turn,
                    delta: resp.content.clone(),
                });
            }
        }
        Ok(resp)
    }

    /// Optional underlying streaming `LLMClientTrait` (used by `Summarizer` and `CacheWarmer`).
    fn as_stream_client(&self) -> Option<Arc<dyn LLMClientTrait>> {
        None
    }
}

/// Adapter bridging any [`LLMClientTrait`] (such as `thunder_providers::RpiAiClient`) into [`LlmDriver`].
pub struct ProviderClientDriver {
    client: Arc<dyn LLMClientTrait>,
    model: Option<String>,
    thinking_level: Option<String>,
    session_id: Option<String>,
}

impl ProviderClientDriver {
    pub fn new(client: Arc<dyn LLMClientTrait>) -> Self {
        Self {
            client,
            model: None,
            thinking_level: None,
            session_id: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.thinking_level = Some(level.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

#[async_trait]
impl LlmDriver for ProviderClientDriver {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDescriptor],
    ) -> Result<LlmTurnResponse, String> {
        let cancel = CancellationToken::new();
        self.complete_streaming(messages, tools, 1, None, &cancel)
            .await
    }

    async fn complete_streaming(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDescriptor],
        turn: usize,
        event_sink: Option<&Arc<dyn thunder_protocol::EventSink>>,
        cancel: &CancellationToken,
    ) -> Result<LlmTurnResponse, String> {
        let tool_defs: Vec<ToolDefinition> = tools.iter().map(ToolDefinition::from).collect();
        let options = ChatRequestOptions {
            messages: messages.to_vec(),
            tools: tool_defs,
            model: self.model.clone(),
            temperature: None,
            top_p: None,
            max_tokens: None,
            thinking_level: self.thinking_level.clone(),
            cache_retention: None,
            session_id: self.session_id.clone(),
        };

        let mut rx = self.client.stream_chat(options, cancel.clone()).await?;

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut prompt_tokens = 0usize;
        let mut completion_tokens = 0usize;
        let mut cached_tokens = 0usize;

        while let Some(chunk) = rx.recv().await {
            match chunk? {
                LLMStreamChunk::Token(delta) => {
                    if let Some(sink) = event_sink {
                        sink.on_agent_event(thunder_protocol::AgentEvent::TokenDelta {
                            turn,
                            delta: delta.clone(),
                        });
                    }
                    content.push_str(&delta);
                }
                LLMStreamChunk::ReasoningToken(delta) => {
                    if let Some(sink) = event_sink {
                        sink.on_agent_event(thunder_protocol::AgentEvent::ReasoningDelta {
                            turn,
                            delta: delta.clone(),
                        });
                    }
                    reasoning.push_str(&delta);
                }
                LLMStreamChunk::ToolCallChunk(_) => {}
                LLMStreamChunk::Completed {
                    content: final_content,
                    tool_calls: wire_calls,
                    prompt_tokens: pt,
                    completion_tokens: ct,
                    cached_tokens: cached,
                    ..
                } => {
                    if let Some(c) = final_content {
                        if !c.is_empty() && content.is_empty() {
                            if let Some(sink) = event_sink {
                                sink.on_agent_event(thunder_protocol::AgentEvent::TokenDelta {
                                    turn,
                                    delta: c.clone(),
                                });
                            }
                            content = c;
                        } else if !c.is_empty() {
                            content = c;
                        }
                    }
                    tool_calls = wire_calls
                        .iter()
                        .map(ToolCallRequest::from_wire_tool_call)
                        .collect();
                    prompt_tokens = pt.unwrap_or(0);
                    completion_tokens = ct.unwrap_or(0);
                    cached_tokens = cached.unwrap_or(0);
                }
            }
        }

        Ok(LlmTurnResponse {
            content,
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            tool_calls,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
        })
    }

    fn as_stream_client(&self) -> Option<Arc<dyn LLMClientTrait>> {
        Some(Arc::clone(&self.client))
    }
}

/// Deterministic scripted mock driver for testing the ReAct Agent Loop, checkpoint compaction, and KV-Cache invariance.
pub struct ScriptedMockDriver {
    responses: Mutex<VecDeque<Result<LlmTurnResponse, String>>>,
    summary_response: Mutex<String>,
    recorded_tool_prefixes: Mutex<Vec<String>>,
    recorded_messages: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedMockDriver {
    pub fn new(steps: Vec<LlmTurnResponse>) -> Self {
        Self {
            responses: Mutex::new(steps.into_iter().map(Ok).collect()),
            summary_response: Mutex::new(
                "## Goal\nTest checkpoint\n\n## Progress\n### Done\n- [x] Step 1".to_string(),
            ),
            recorded_tool_prefixes: Mutex::new(Vec::new()),
            recorded_messages: Mutex::new(Vec::new()),
        }
    }

    pub fn with_results(steps: Vec<Result<LlmTurnResponse, String>>) -> Self {
        Self {
            responses: Mutex::new(steps.into()),
            summary_response: Mutex::new(
                "## Goal\nRecovered from context overflow\n\n## Progress\n### Done\n- [x] Compacted".to_string(),
            ),
            recorded_tool_prefixes: Mutex::new(Vec::new()),
            recorded_messages: Mutex::new(Vec::new()),
        }
    }

    pub fn recorded_tool_prefixes(&self) -> Vec<String> {
        self.recorded_tool_prefixes.lock().clone()
    }

    pub fn recorded_messages(&self) -> Vec<Vec<ChatMessage>> {
        self.recorded_messages.lock().clone()
    }
}

#[async_trait]
impl LLMClientTrait for ScriptedMockDriver {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = mpsc::channel(4);
        let summary = self.summary_response.lock().clone();
        let _ = tx
            .send(Ok(LLMStreamChunk::Completed {
                content: Some(summary),
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                prompt_tokens: Some(64),
                completion_tokens: Some(32),
                cached_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }))
            .await;
        Ok(rx)
    }
}

#[async_trait]
impl LlmDriver for ScriptedMockDriver {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDescriptor],
    ) -> Result<LlmTurnResponse, String> {
        let serialized_tools =
            serde_json::to_string(tools).map_err(|e| format!("Failed to serialize tools: {e}"))?;
        self.recorded_tool_prefixes.lock().push(serialized_tools);
        self.recorded_messages.lock().push(messages.to_vec());

        self.responses
            .lock()
            .pop_front()
            .ok_or_else(|| "ScriptedMockDriver exhausted all scripted responses".to_string())?
    }
}
