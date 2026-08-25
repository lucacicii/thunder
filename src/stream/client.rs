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
use tracing::{debug, info, warn};

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
    max_retries: usize,
    chunk_idle_timeout: Duration,
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

        // Only connect-level timeout is set on the client.
        let connect_timeout = Duration::from_millis(config.request_timeout_ms.min(30_000));
        let chunk_idle_timeout = Duration::from_millis(config.request_timeout_ms.max(60_000));

        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .pool_max_idle_per_host(10)
            .tcp_nodelay(true)
            .build()
            .unwrap_or_default();

        Self {
            client,
            api_base: config.api_base.trim_end_matches('/').to_string(),
            default_model: config.model.clone(),
            headers,
            max_retries: 3,
            chunk_idle_timeout,
        }
    }

    /// Reuse a host-owned HTTP client (proxy, mTLS, shared pool).
    pub fn from_client(config: &AgentConfig, client: reqwest::Client) -> Self {
        let mut built = Self::new(config);
        built.client = client;
        built
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
            "stream": true
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

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let client = self.client.clone();
        let headers = self.headers.clone();
        let max_retries = self.max_retries;
        let chunk_idle_timeout = self.chunk_idle_timeout;

        tokio::spawn(async move {
            let mut attempt = 0;

            while attempt <= max_retries {
                attempt += 1;
                debug!(
                    attempt = attempt,
                    url = %url,
                    model = %model,
                    "Dispatching streaming LLM request"
                );

                let request = client.post(&url).headers(headers.clone()).json(&payload);

                let response_res = tokio::select! {
                    res = request.send() => res,
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Err("Request cancelled by user".to_string())).await;
                        return;
                    }
                };

                let response = match response_res {
                    Ok(resp) => resp,
                    Err(err) => {
                        warn!(
                            attempt = attempt,
                            error = %err,
                            "Network connection failed during LLM request"
                        );
                        if attempt <= max_retries {
                            let backoff = Duration::from_millis(500 * (1 << (attempt - 1)));
                            tokio::time::sleep(backoff).await;
                            continue;
                        } else {
                            let _ = tx.send(Err(format!("HTTP connection failed after {} attempts: {}", max_retries, err))).await;
                            return;
                        }
                    }
                };

                let status = response.status();
                if !status.is_success() {
                    // Respect server-provided Retry-After when present
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs);

                    let error_body = response.text().await.unwrap_or_default();
                    warn!(
                        status = %status,
                        attempt = attempt,
                        body = %error_body,
                        "LLM API returned error response"
                    );

                    // Transient errors eligible for retry: 429, 500, 502, 503, 504
                    if (status.as_u16() == 429 || status.is_server_error()) && attempt <= max_retries {
                        let backoff =
                            retry_after.unwrap_or_else(|| Duration::from_millis(1000 * (1 << (attempt - 1))));
                        info!(backoff_ms = backoff.as_millis(), "Retrying after transient error");
                        tokio::time::sleep(backoff).await;
                        continue;
                    }

                    let _ = tx
                        .send(Err(format!("LLM API returned HTTP {}: {}", status, error_body)))
                        .await;
                    return;
                }

                let mut byte_stream = response.bytes_stream();
                let mut parser = SSEStreamParser::new();
                let mut last_finish_reason = "stop".to_string();
                let mut prompt_tokens = None;
                let mut completion_tokens = None;
                let mut stream_failed_midway = false;

                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            let _ = tx.send(Err("Stream aborted by cancellation".to_string())).await;
                            return;
                        }
                        // Chunk idle timeout: if no bytes arrive for chunk_idle_timeout, trigger retry
                        chunk_res = tokio::time::timeout(chunk_idle_timeout, byte_stream.next()) => {
                            match chunk_res {
                                Ok(Some(Ok(bytes))) => {
                                    for delta in parser.feed_chunk(&bytes) {
                                        if let Some(text) = delta.content_delta {
                                            if tx.send(Ok(LLMStreamChunk::Token(text))).await.is_err() {
                                                return;
                                            }
                                        }
                                        if let Some(reasoning) = delta.reasoning_delta {
                                            if tx.send(Ok(LLMStreamChunk::ReasoningToken(reasoning))).await.is_err() {
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
                                Ok(Some(Err(err))) => {
                                    warn!(
                                        attempt = attempt,
                                        error = %err,
                                        "Stream interrupted or decoding failed midway"
                                    );
                                    stream_failed_midway = true;
                                    break;
                                }
                                Ok(None) => {
                                    // Clean end of stream
                                    break;
                                }
                                Err(_) => {
                                    warn!(
                                        attempt = attempt,
                                        timeout_secs = chunk_idle_timeout.as_secs(),
                                        "Chunk read timed out (no data received within idle timeout window)"
                                    );
                                    stream_failed_midway = true;
                                    break;
                                }
                            }
                        }
                    }
                }

                if stream_failed_midway {
                    let _ = tx
                        .send(Err(format!(
                            "Network stream error: connection dropped or response decoding timed out"
                        )))
                        .await;
                    return;
                }

                if let Some(flushed) = parser.flush() {
                    if let Some(text) = flushed.content_delta {
                        let _ = tx.send(Ok(LLMStreamChunk::Token(text))).await;
                    }
                    if let Some(reasoning) = flushed.reasoning_delta {
                        let _ = tx.send(Ok(LLMStreamChunk::ReasoningToken(reasoning))).await;
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

                debug!(
                    tool_calls_count = tool_calls.len(),
                    finish_reason = %last_finish_reason,
                    "Streaming chunk response completed"
                );

                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: None,
                        tool_calls,
                        finish_reason: last_finish_reason,
                        prompt_tokens,
                        completion_tokens,
                    }))
                    .await;
                return;
            }
        });

        Ok(rx)
    }
}
