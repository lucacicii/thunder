use crate::adapters::ModelAdapter;
use crate::catalog::ModelSpec;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use tokio_util::sync::CancellationToken;

/// Adapter implementing Anthropic's Claude Messages API
pub struct AnthropicAdapter;

impl ModelAdapter for AnthropicAdapter {
    fn name(&self) -> &'static str {
        "anthropic-messages"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let (system, messages) = split_system(&options.messages);
        let mut payload = json!({
            "model": options.model.clone().unwrap_or_else(|| spec.id.clone()),
            "max_tokens": options.max_tokens.unwrap_or(spec.max_tokens),
            "stream": true,
            "messages": messages,
        });

        if let Some(system_text) = system {
            // Anthropic Prompt Caching: inject cache_control on system prompt block
            payload["system"] = json!([
                {
                    "type": "text",
                    "text": system_text,
                    "cache_control": { "type": "ephemeral" }
                }
            ]);
        }

        if let Some(temp) = options.temperature {
            payload["temperature"] = json!(temp);
        }

        if !options.tools.is_empty() {
            let mut tools_json = Vec::new();
            let total = options.tools.len();
            for (idx, t) in options.tools.iter().enumerate() {
                let mut tool_obj = json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": t.function.parameters,
                });
                // Place cache_control on the final tool to cache all tool definitions
                if idx == total - 1 {
                    tool_obj["cache_control"] = json!({ "type": "ephemeral" });
                }
                tools_json.push(tool_obj);
            }
            payload["tools"] = json!(tools_json);
        }

        payload
    }
}

pub struct AnthropicClient {
    http: reqwest::Client,
    spec: ModelSpec,
    adapter: AnthropicAdapter,
}

impl AnthropicClient {
    pub fn new(spec: ModelSpec) -> Self {
        Self {
            http: reqwest::Client::new(),
            spec,
            adapter: AnthropicAdapter,
        }
    }
}

#[async_trait]
impl LLMClientTrait for AnthropicClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let base_url = if self.spec.base_url.trim().is_empty() {
            "https://api.anthropic.com".to_string()
        } else {
            self.spec.base_url.trim_end_matches('/').to_string()
        };
        let url = format!("{}/v1/messages", base_url);
        let payload = self.adapter.adapt_payload(&self.spec, &options);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert("anthropic-beta", HeaderValue::from_static("prompt-caching-2024-07-25"));
        if let Some(key) = &self.spec.api_key {
            if let Ok(val) = HeaderValue::from_str(key) {
                headers.insert("x-api-key", val);
            }
        }
        for (k, v) in &self.spec.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, val);
            }
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let http = self.http.clone();

        tokio::spawn(async move {
            let request = http.post(url).headers(headers).json(&payload);
            let response = tokio::select! {
                res = request.send() => res,
                _ = cancel_token.cancelled() => {
                    let _ = tx.send(Err("Request cancelled by user".to_string())).await;
                    return;
                }
            };

            let response = match response {
                Ok(resp) => resp,
                Err(err) => {
                    let _ = tx.send(Err(err.to_string())).await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let _ = tx
                    .send(Err(format!("Anthropic API returned HTTP {status}: {body}")))
                    .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buf = String::new();
            let mut content = String::new();
            let mut tool_calls = Vec::new();
            let mut current_tool: Option<(String, String, String)> = None;
            let mut finish = "end_turn".to_string();
            let mut prompt_tokens = None;
            let mut completion_tokens = None;
            let mut cached_tokens = None;

            while let Some(chunk) = stream.next().await {
                if cancel_token.is_cancelled() {
                    let _ = tx.send(Err("Stream aborted by cancellation".to_string())).await;
                    return;
                }
                let Ok(bytes) = chunk else { continue };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(idx) = buf.find("\n\n") {
                    let frame = buf[..idx].to_string();
                    buf = buf[idx + 2..].to_string();
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data: ") else { continue };
                        let data = data.trim();
                        if data == "[DONE]" {
                            continue;
                        }
                        let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
                        match value.get("type").and_then(|v| v.as_str()) {
                            Some("content_block_delta") => {
                                if let Some(text) = value.pointer("/delta/text").and_then(|v| v.as_str()) {
                                    content.push_str(text);
                                    let _ = tx.send(Ok(LLMStreamChunk::Token(text.to_string()))).await;
                                }
                                if let Some(thinking) = value.pointer("/delta/thinking").and_then(|v| v.as_str()) {
                                    let _ = tx
                                        .send(Ok(LLMStreamChunk::ReasoningToken(thinking.to_string())))
                                        .await;
                                }
                                if let Some(partial) = value.pointer("/delta/partial_json").and_then(|v| v.as_str()) {
                                    if let Some((_, _, args)) = current_tool.as_mut() {
                                        args.push_str(partial);
                                    }
                                }
                            }
                            Some("content_block_start") => {
                                if value.pointer("/content_block/type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    let id = value
                                        .pointer("/content_block/id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("tool")
                                        .to_string();
                                    let name = value
                                        .pointer("/content_block/name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("tool")
                                        .to_string();
                                    current_tool = Some((id, name, String::new()));
                                }
                            }
                            Some("content_block_stop") => {
                                if let Some((id, name, args)) = current_tool.take() {
                                    tool_calls.push(ToolCall::new_function(id, name, args));
                                }
                            }
                            Some("message_start") => {
                                if let Some(usage) = value.pointer("/message/usage") {
                                    if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                                        prompt_tokens = Some(input as usize);
                                    }
                                    if let Some(cached) = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                                        cached_tokens = Some(cached as usize);
                                    }
                                }
                            }
                            Some("message_delta") => {
                                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                                    finish = reason.to_string();
                                }
                                if let Some(usage) = value.get("usage") {
                                    if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                                        completion_tokens = Some(output as usize);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: if content.is_empty() { None } else { Some(content) },
                    tool_calls,
                    finish_reason: finish,
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                }))
                .await;
        });

        Ok(rx)
    }
}

pub fn split_system(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system = None;
    let mut out = Vec::new();
    for message in messages {
        match message {
            ChatMessage::System { content, .. } => {
                system = Some(match system.take() {
                    Some(existing) => format!("{existing}\n\n{content}"),
                    None => content.clone(),
                });
            }
            ChatMessage::User { content, .. } => {
                out.push(json!({"role": "user", "content": content}));
            }
            ChatMessage::Assistant { content, tool_calls, .. } => {
                if let Some(calls) = tool_calls {
                    let blocks: Vec<Value> = calls
                        .iter()
                        .map(|call| {
                            json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.function.name,
                                "input": serde_json::from_str::<Value>(&call.function.arguments).unwrap_or(json!({}))
                            })
                        })
                        .collect();
                    out.push(json!({"role": "assistant", "content": blocks}));
                } else {
                    out.push(json!({"role": "assistant", "content": content.clone().unwrap_or_default()}));
                }
            }
            ChatMessage::Tool { tool_call_id, content, .. } => {
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content
                    }]
                }));
            }
        }
    }
    (system, out)
}
