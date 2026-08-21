use crate::stream::sse::{SSEStreamParser, StreamToolCallDelta};
use crate::types::config::AgentConfig;
use crate::types::message::{ChatMessage, ToolCall};
use crate::types::tool::ToolDefinition;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum LLMStreamChunk {
    Token(String),
    ToolCallChunk(StreamToolCallDelta),
    Completed {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
        finish_reason: String,
        prompt_tokens: Option<usize>,
        completion_tokens: Option<usize>,
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
}

#[async_trait]
pub trait LLMClientTrait: Send + Sync {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String>;
}

#[derive(Clone)]
pub struct LLMClient {
    client: reqwest::Client,
    api_base: String,
    default_model: String,
    headers: HeaderMap,
}

impl LLMClient {
    pub fn new(config: &AgentConfig) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(key) = &config.api_key {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", key)) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        for (k, v) in &config.headers {
            if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                headers.insert(name, val);
            }
        }

        let timeout = Duration::from_millis(config.request_timeout_ms);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .pool_max_idle_per_host(10)
            .tcp_nodelay(true)
            .build()
            .unwrap_or_default();

        Self {
            client,
            api_base: config.api_base.trim_end_matches('/').to_string(),
            default_model: config.model.clone(),
            headers,
        }
    }
}

#[async_trait]
impl LLMClientTrait for LLMClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let url = format!("{}/chat/completions", self.api_base);
        let model = options.model.unwrap_or_else(|| self.default_model.clone());

        let mut payload = json!({
            "model": model,
            "messages": options.messages,
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !options.tools.is_empty() {
            payload["tools"] = json!(options.tools);
            payload["tool_choice"] = json!("auto");
        }

        if let Some(temp) = options.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(top_p) = options.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(max_tokens) = options.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }

        let request = self
            .client
            .post(&url)
            .headers(self.headers.clone())
            .json(&payload);

        let response = tokio::select! {
            res = request.send() => {
                res.map_err(|e| format!("HTTP request failed: {}", e))?
            }
            _ = cancel_token.cancelled() => {
                return Err("Request cancelled".to_string());
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("LLM API returned HTTP {}: {}", status, body));
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            let mut byte_stream = response.bytes_stream();
            let mut parser = SSEStreamParser::new();
            let mut accumulated_content = String::new();
            let mut last_finish_reason = "stop".to_string();
            let mut prompt_tokens = None;
            let mut completion_tokens = None;

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Err("Stream aborted by cancellation".to_string())).await;
                        break;
                    }
                    chunk_opt = byte_stream.next() => {
                        match chunk_opt {
                            Some(Ok(bytes)) => {
                                for delta in parser.feed_chunk(&bytes) {
                                    if let Some(text) = delta.content_delta {
                                        accumulated_content.push_str(&text);
                                        if tx.send(Ok(LLMStreamChunk::Token(text))).await.is_err() {
                                            return;
                                        }
                                    }

                                    if let Some(tc_deltas) = delta.tool_calls_delta {
                                        for tc in tc_deltas {
                                            if tx.send(Ok(LLMStreamChunk::ToolCallChunk(tc))).await.is_err() {
                                                return;
                                            }
                                        }
                                    }

                                    if let Some(fr) = delta.finish_reason {
                                        last_finish_reason = fr;
                                    }

                                    if delta.prompt_tokens.is_some() {
                                        prompt_tokens = delta.prompt_tokens;
                                    }
                                    if delta.completion_tokens.is_some() {
                                        completion_tokens = delta.completion_tokens;
                                    }
                                }
                            }
                            Some(Err(err)) => {
                                let _ = tx.send(Err(format!("Network stream error: {}", err))).await;
                                return;
                            }
                            None => {
                                break;
                            }
                        }
                    }
                }
            }

            if let Some(flushed) = parser.flush() {
                if let Some(text) = flushed.content_delta {
                    accumulated_content.push_str(&text);
                    let _ = tx.send(Ok(LLMStreamChunk::Token(text))).await;
                }
                if let Some(fr) = flushed.finish_reason {
                    last_finish_reason = fr;
                }
                if flushed.prompt_tokens.is_some() {
                    prompt_tokens = flushed.prompt_tokens;
                }
                if flushed.completion_tokens.is_some() {
                    completion_tokens = flushed.completion_tokens;
                }
            }

            let tool_calls = parser.get_completed_tool_calls();
            if !tool_calls.is_empty() && last_finish_reason == "stop" {
                last_finish_reason = "tool_calls".to_string();
            }

            let content = if accumulated_content.is_empty() {
                None
            } else {
                Some(accumulated_content)
            };

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content,
                    tool_calls,
                    finish_reason: last_finish_reason,
                    prompt_tokens,
                    completion_tokens,
                }))
                .await;
        });

        Ok(rx)
    }
}
