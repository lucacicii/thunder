//! `RpiAiClient`: the `LLMClientTrait` implementation that streams over HTTP
//! (`rpi-ai`). No sidecar process, no Node.
//!
//! Where the wire could drift, this follows the conventions thunder's loop and
//! the panel's usage accounting were built against — the ones the retired
//! `thunder-pi-bridge` sidecar established:
//!
//! * **`prompt_tokens` is the full prompt.** rpi-ai reports `input` as the
//!   *uncached* portion only, so the cache buckets are folded back in.
//! * **`finish_reason` keeps the bridge's vocabulary, not rpi-ai's**:
//!   `toolUse`/`deferred` → `tool_calls`, `length` → `length`, else `stop`.
//! * **Tool calls arrive whole on the terminal event**, never as deltas, so
//!   `LLMStreamChunk::ToolCallChunk` stays unused — as it was.
//!
//! Known gaps, reported rather than papered over:
//! * `top_p` has no first-class field in `rpi-ai`; it rides on
//!   `Model::sampling_params`, which the OpenAI-shaped providers merge straight
//!   into the request body. The Anthropic provider ignores that map.
//! * Only `openai-responses`, `openai-completions` and `anthropic-messages` are
//!   wired; [`crate::convert::supports_api`] is the gate.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rpi_ai::providers::anthropic::AnthropicProvider;
use rpi_ai::providers::openai_completions::OpenAiCompletionsProvider;
use rpi_ai::providers::openai_responses::OpenAiResponsesProvider;
use rpi_ai::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Content, Context, DoneReason,
    ImageContent, Message, Model, Provider, SimpleStreamOptions, Tool, ToolResultMessage,
    UserContent, UserMessage,
};
// Not re-exported at the crate root, unlike its siblings in `types`.
use rpi_ai::types::ToolResultRole;
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::{ChatMessage, ContentPart, ToolCall};
use thunder_agent_loop::types::tool::ToolDefinition;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::convert::{level_from_str, model_from_descriptor, ConversionNotes};
use crate::model::ModelDescriptor;

/// Channel depth for one stream's chunks.
const CHUNK_CHANNEL: usize = 64;

pub struct RpiAiClient {
    provider: Arc<dyn Provider>,
    model: Model,
    timeout: Duration,
}

impl RpiAiClient {
    /// Build a client from the descriptor a host sends to `thunder-runtime`.
    ///
    /// Returns the conversion notes alongside, because one field genuinely does
    /// not survive the trip (see [`ConversionNotes`]).
    pub fn from_descriptor(
        model: &ModelDescriptor,
        timeout_ms: u64,
    ) -> Result<(Self, ConversionNotes), String> {
        let (converted, notes) = model_from_descriptor(model)?;
        let client = Self::with_model(
            model.provider.clone(),
            model.api.clone(),
            model.api_key.clone(),
            model.base_url.clone(),
            converted,
            timeout_ms,
        )?;
        Ok((client, notes))
    }

    fn with_model(
        provider_id: String,
        api: String,
        api_key: Option<String>,
        base_url: String,
        model: Model,
        timeout_ms: u64,
    ) -> Result<Self, String> {
        let http = rpi_ai::http::build_client(&base_url, None, None)?;
        let provider: Arc<dyn Provider> = match api.as_str() {
            "openai-responses" => {
                Arc::new(OpenAiResponsesProvider::with_models_without_env_api_key(
                    provider_id,
                    api_key,
                    http,
                    vec![model.clone()],
                ))
            }
            "openai-completions" => {
                Arc::new(OpenAiCompletionsProvider::with_models_without_env_api_key(
                    provider_id,
                    api_key,
                    http,
                    vec![model.clone()],
                ))
            }
            "anthropic-messages" => Arc::new(AnthropicProvider::with_models_without_env_api_key(
                api_key,
                http,
                vec![model.clone()],
            )),
            other => return Err(crate::convert::unsupported_api_message(other)),
        };
        Ok(Self {
            provider,
            model,
            timeout: Duration::from_millis(timeout_ms.max(1)),
        })
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Test seam: swap the provider for an in-process one (`faux`), which is why
    /// this client's tests need no network at all.
    pub fn with_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = provider;
        self
    }
}

#[async_trait]
impl LLMClientTrait for RpiAiClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let mut model = self.model.clone();
        // `ChatRequestOptions::model` is deliberately ignored: no caller fills it
        // in (the loop pins the model at client construction), so honouring it
        // here would only invent a second source of truth.
        // `top_p` has no home in `SimpleStreamOptions`; the OpenAI-shaped
        // providers merge `sampling_params` into the request body verbatim.
        if let Some(top_p) = options.top_p {
            let params = model.sampling_params.get_or_insert_with(Default::default);
            params.insert("top_p".into(), json!(top_p));
        }

        let ctx = build_context(&options, &model)?;
        let opts = build_options(&options, self.timeout, &cancel_token);

        let (tx, rx) = mpsc::channel::<Result<LLMStreamChunk, String>>(CHUNK_CHANNEL);
        let provider = Arc::clone(&self.provider);

        tokio::spawn(async move {
            let mut stream = provider.stream_simple(&model, &ctx, &opts).await;
            let mut text = String::new();

            while let Some(event) = stream.next().await {
                match event {
                    AssistantMessageEvent::TextDelta { delta, .. } => {
                        text.push_str(&delta);
                        if tx.send(Ok(LLMStreamChunk::Token(delta))).await.is_err() {
                            return;
                        }
                    }
                    AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        if tx
                            .send(Ok(LLMStreamChunk::ReasoningToken(delta)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    // Tool-call deltas are deliberately dropped: consumers expect
                    // the call whole on the terminal event, never split.
                    AssistantMessageEvent::Done { reason, message } => {
                        let _ = tx.send(Ok(completed(reason, &message))).await;
                        return;
                    }
                    AssistantMessageEvent::Error { error, .. } => {
                        let _ = tx.send(Err(error_message(&error))).await;
                        return;
                    }
                    _ => {}
                }
            }

            // The stream ended without a terminal event: a producer bug, not a
            // model outcome. Surface it instead of closing the channel silently.
            let _ = tx
                .send(Err(format!(
                    "rpi-ai stream ended without a terminal event ({} chars of text received)",
                    text.chars().count()
                )))
                .await;
        });

        Ok(rx)
    }
}

// ---------------------------------------------------------------------------
// Request mapping
// ---------------------------------------------------------------------------

fn build_options(
    options: &ChatRequestOptions,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> SimpleStreamOptions {
    let signal = CancellationToken::new();
    // rpi-ai honours its own token between chunks; bridge the caller's over.
    let caller = cancel_token.clone();
    let signal_for_task = signal.clone();
    tokio::spawn(async move {
        caller.cancelled().await;
        signal_for_task.cancel();
    });

    SimpleStreamOptions {
        reasoning: options.thinking_level.as_deref().and_then(level_from_str),
        cache_retention: cache_retention(options.cache_retention.as_deref()),
        session_id: options.session_id.clone(),
        temperature: options.temperature.map(f64::from),
        max_tokens: options.max_tokens.map(|m| m as u64),
        timeout: Some(timeout),
        signal,
        ..Default::default()
    }
}

/// pi-ai's `cacheRetention`, spelled the way thunder spells it. Absent means
/// "let the transport decide", which is pi-ai's documented default of `short`.
fn cache_retention(value: Option<&str>) -> CacheRetention {
    match value {
        Some("none") => CacheRetention::None,
        Some("long") => CacheRetention::Long,
        Some("short") | None => CacheRetention::Short,
        Some(_) => CacheRetention::Short,
    }
}

fn build_context(options: &ChatRequestOptions, model: &Model) -> Result<Context, String> {
    let mut system_prompt: Option<String> = None;
    let mut messages = Vec::new();

    for message in &options.messages {
        match message {
            ChatMessage::System { content, .. } => {
                system_prompt = Some(match system_prompt.take() {
                    Some(existing) => format!("{existing}\n\n{content}"),
                    None => content.clone(),
                });
            }
            ChatMessage::User { content, parts, .. } => {
                let body = match parts {
                    Some(parts) if !parts.is_empty() => {
                        UserContent::Blocks(parts.iter().map(part_to_content).collect())
                    }
                    _ => UserContent::Text(content.clone()),
                };
                messages.push(Message::User(UserMessage::new(body, now_ms())));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut blocks: Vec<Content> = Vec::new();
                if let Some(text) = content.as_deref().filter(|c| !c.is_empty()) {
                    blocks.push(Content::text(text));
                }
                for call in tool_calls.iter().flatten() {
                    blocks.push(Content::tool_call(
                        call.id.clone(),
                        call.function.name.clone(),
                        arguments_value(&call.function.arguments),
                    ));
                }
                // An assistant turn with neither text nor tool calls carries no
                // information and would serialize as an empty content array,
                // which some providers reject.
                if blocks.is_empty() {
                    continue;
                }
                let mut msg = AssistantMessage::empty(
                    model.api.clone(),
                    model.provider.clone(),
                    model.id.clone(),
                    now_ms(),
                );
                msg.content = blocks;
                messages.push(Message::Assistant(Box::new(msg)));
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
                name,
            } => {
                messages.push(Message::ToolResult(Box::new(ToolResultMessage {
                    role: ToolResultRole,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: name.clone().unwrap_or_default(),
                    content: vec![Content::text(content)],
                    details: None,
                    usage: None,
                    added_tool_names: Vec::new(),
                    is_error: false,
                    timestamp: now_ms(),
                })));
            }
        }
    }

    Ok(Context {
        system_prompt,
        messages,
        tools: options.tools.iter().map(tool_to_rpi).collect(),
    })
}

fn part_to_content(part: &ContentPart) -> Content {
    match part {
        ContentPart::Text { text } => Content::text(text),
        ContentPart::Image {
            mime_type, data, ..
        } => Content::Image(ImageContent {
            kind: Default::default(),
            data: data.clone(),
            mime_type: mime_type.clone(),
        }),
    }
}

fn tool_to_rpi(tool: &ToolDefinition) -> Tool {
    Tool {
        name: tool.function.name.clone(),
        description: tool.function.description.clone(),
        parameters: tool.function.parameters.clone().into(),
        constrained_sampling: None,
    }
}

/// Tool arguments arrive as a JSON *string* from the loop. Malformed JSON is
/// kept as a string rather than dropped: the providers treat a non-object as
/// "no arguments", which is a better failure mode than losing the call.
fn arguments_value(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

// ---------------------------------------------------------------------------
// Response mapping
// ---------------------------------------------------------------------------

fn completed(reason: DoneReason, message: &AssistantMessage) -> LLMStreamChunk {
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in &message.content {
        match block {
            Content::Text(text) => content.push_str(&text.text),
            Content::ToolCall(call) => tool_calls.push(ToolCall::new_function(
                call.id.clone(),
                call.name.clone(),
                call.arguments.to_string(),
            )),
            _ => {}
        }
    }

    let usage = &message.usage;
    LLMStreamChunk::Completed {
        content: (!content.is_empty()).then_some(content),
        tool_calls,
        finish_reason: finish_reason(reason),
        // rpi-ai's `input` excludes the cached buckets; thunder's `prompt_tokens`
        // is the whole prompt, so both buckets are folded back in.
        prompt_tokens: Some((usage.input + usage.cache_read + usage.cache_write) as usize),
        completion_tokens: Some(usage.output as usize),
        cached_tokens: Some(usage.cache_read as usize),
        cache_write_tokens: Some(usage.cache_write as usize),
        reasoning_tokens: usage.reasoning.map(|r| r as usize),
    }
}

/// Keeps the finish-reason vocabulary downstream consumers were built against.
fn finish_reason(reason: DoneReason) -> String {
    match reason {
        DoneReason::ToolUse | DoneReason::Deferred => "tool_calls",
        DoneReason::Length => "length",
        DoneReason::Stop => "stop",
    }
    .to_string()
}

fn error_message(message: &AssistantMessage) -> String {
    message
        .error_message
        .clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("rpi-ai stream error ({:?})", message.stop_reason))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
