use crate::catalog::ModelSpec;
use crate::openai::normalize_openai_base;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use tokio_util::sync::CancellationToken;
use tracing::error;

pub struct OpenAiResponsesClient {
    http: reqwest::Client,
    spec: ModelSpec,
}

impl OpenAiResponsesClient {
    pub fn new(spec: ModelSpec) -> Self {
        Self {
            http: reqwest::Client::new(),
            spec,
        }
    }
}

#[async_trait]
impl LLMClientTrait for OpenAiResponsesClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let base = normalize_openai_base(&self.spec.base_url);
        if base.is_empty() {
            return Err("openai-responses model has no base URL configured".to_string());
        }
        let url = format!("{base}/responses");
        let payload = build_responses_payload(&self.spec.id, &options);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = &self.spec.api_key {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(AUTHORIZATION, val);
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
        let request_url = url.clone();
        tokio::spawn(async move {
            let request = http.post(&request_url).headers(headers).json(&payload);
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
                    let mut full_err = err.to_string();
                    let mut source = std::error::Error::source(&err);
                    while let Some(s) = source {
                        full_err.push_str(&format!(": {s}"));
                        source = std::error::Error::source(s);
                    }
                    error!(url = %request_url, error = %full_err, "Failed to send HTTP request to Responses API");
                    let _ = tx.send(Err(full_err)).await;
                    return;
                }
            };
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let _ = tx
                    .send(Err(format!("OpenAI Responses API returned HTTP {status}: {body}")))
                    .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buf = String::new();
            let mut content = String::new();
            let mut tool_calls = Vec::new();
            let mut finish = "stop".to_string();
            let mut prompt_tokens: Option<usize> = None;
            let mut completion_tokens: Option<usize> = None;
            let mut cached_tokens: Option<usize> = None;

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
                        if data == "[DONE]" {
                            continue;
                        }
                        let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
                        match value.get("type").and_then(|v| v.as_str()) {
                            Some("response.output_text.delta") => {
                                if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                                    content.push_str(delta);
                                    let _ = tx.send(Ok(LLMStreamChunk::Token(delta.to_string()))).await;
                                }
                            }
                            Some("response.reasoning.delta") => {
                                if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                                    let _ = tx
                                        .send(Ok(LLMStreamChunk::ReasoningToken(delta.to_string())))
                                        .await;
                                }
                            }
                            Some("response.output_item.done") => {
                                if value.pointer("/item/type").and_then(|v| v.as_str())
                                    == Some("function_call")
                                {
                                    let id = value
                                        .pointer("/item/call_id")
                                        .or_else(|| value.pointer("/item/id"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("call")
                                        .to_string();
                                    let name = value
                                        .pointer("/item/name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("tool")
                                        .to_string();
                                    let args = value
                                        .pointer("/item/arguments")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("{}")
                                        .to_string();
                                    tool_calls.push(ToolCall::new_function(id, name, args));
                                }
                            }
                            Some("response.completed") => {
                                finish = "stop".to_string();
                            }
                            _ => {}
                        }

                        // Parse usage information from any event or chunk
                        let usage_node = value.get("usage").or_else(|| value.pointer("/response/usage"));
                        if let Some(u) = usage_node {
                            if let Some(pt) = u.get("input_tokens").or_else(|| u.get("prompt_tokens")).and_then(|v| v.as_u64()) {
                                prompt_tokens = Some(pt as usize);
                            }
                            if let Some(ct) = u.get("output_tokens").or_else(|| u.get("completion_tokens")).and_then(|v| v.as_u64()) {
                                completion_tokens = Some(ct as usize);
                            }
                            let cached = u.pointer("/input_token_details/cached_tokens")
                                .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
                                .or_else(|| u.get("prompt_cache_hit_tokens"))
                                .or_else(|| u.get("cache_read_input_tokens"))
                                .and_then(|v| v.as_u64());
                            if let Some(v) = cached {
                                cached_tokens = Some(v as usize);
                            }
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

fn build_responses_payload(wire_model: &str, options: &ChatRequestOptions) -> Value {
    let mut input = Vec::new();
    for message in &options.messages {
        match message {
            ChatMessage::System { content, .. } => {
                input.push(json!({"role": "system", "content": content}));
            }
            ChatMessage::User { content, .. } => {
                input.push(json!({"role": "user", "content": content}));
            }
            ChatMessage::Assistant { content, tool_calls, .. } => {
                if let Some(text) = content {
                    if !text.is_empty() {
                        input.push(json!({"role": "assistant", "content": text}));
                    }
                }
                if let Some(calls) = tool_calls {
                    for call in calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.function.name,
                            "arguments": call.function.arguments
                        }));
                    }
                }
            }
            ChatMessage::Tool { tool_call_id, content, .. } => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content
                }));
            }
        }
    }

    let mut payload = json!({
        "model": wire_model,
        "input": input,
        "stream": true,
        "stream_options": { "include_usage": true }
    });
    if let Some(temp) = options.temperature {
        payload["temperature"] = json!(temp);
    }
    if let Some(max_tokens) = options.max_tokens {
        payload["max_output_tokens"] = json!(max_tokens);
    }
    if !options.tools.is_empty() {
        let tools: Vec<serde_json::Value> = options
            .tools
            .iter()
            .map(|t| {
                let mut obj = serde_json::json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters,
                });
                if let Some(strict) = t.function.strict {
                    obj["strict"] = serde_json::json!(strict);
                }
                obj
            })
            .collect();
        payload["tools"] = serde_json::json!(tools);
    }
    payload
}
