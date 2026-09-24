use crate::adapters::ModelAdapter;
use crate::catalog::ModelSpec;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::ChatMessage;
use tokio_util::sync::CancellationToken;

/// Adapter implementing Ollama's native `/api/chat` interface
pub struct OllamaAdapter;

impl ModelAdapter for OllamaAdapter {
    fn name(&self) -> &'static str {
        "ollama-native"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let model = options.model.clone().unwrap_or_else(|| spec.id.clone());

        let messages: Vec<Value> = options
            .messages
            .iter()
            .map(|msg| match msg {
                ChatMessage::System { content, .. } => json!({ "role": "system", "content": content }),
                ChatMessage::User { content, .. } => json!({ "role": "user", "content": content }),
                ChatMessage::Assistant { content, .. } => {
                    json!({ "role": "assistant", "content": content.clone().unwrap_or_default() })
                }
                ChatMessage::Tool { content, .. } => json!({ "role": "tool", "content": content }),
            })
            .collect();

        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "options": {
                "temperature": options.temperature.unwrap_or(0.7),
            }
        });

        if let Some(max_tokens) = options.max_tokens {
            payload["options"]["num_predict"] = json!(max_tokens);
        }

        payload
    }
}

pub struct OllamaClient {
    http: reqwest::Client,
    spec: ModelSpec,
    adapter: OllamaAdapter,
}

impl OllamaClient {
    pub fn new(spec: ModelSpec) -> Self {
        Self {
            http: reqwest::Client::new(),
            spec,
            adapter: OllamaAdapter,
        }
    }
}

#[async_trait]
impl LLMClientTrait for OllamaClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let base_url = if self.spec.base_url.trim().is_empty() {
            "http://localhost:11434".to_string()
        } else {
            self.spec.base_url.trim_end_matches('/').to_string()
        };
        let url = format!("{base_url}/api/chat");
        let payload = self.adapter.adapt_payload(&self.spec, &options);

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let http = self.http.clone();

        tokio::spawn(async move {
            let request = http.post(url).json(&payload);
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
                    .send(Err(format!("Ollama API returned HTTP {status}: {body}")))
                    .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buf = String::new();
            let mut content = String::new();
            let mut prompt_tokens = None;
            let mut completion_tokens = None;

            while let Some(chunk) = stream.next().await {
                if cancel_token.is_cancelled() {
                    let _ = tx.send(Err("Stream aborted by cancellation".to_string())).await;
                    return;
                }
                let Ok(bytes) = chunk else { continue };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(idx) = buf.find('\n') {
                    let line = buf[..idx].trim().to_string();
                    buf = buf[idx + 1..].to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        if let Some(text) = value.pointer("/message/content").and_then(|v| v.as_str()) {
                            content.push_str(text);
                            let _ = tx.send(Ok(LLMStreamChunk::Token(text.to_string()))).await;
                        }
                        if let Some(pt) = value.get("prompt_eval_count").and_then(|v| v.as_u64()) {
                            prompt_tokens = Some(pt as usize);
                        }
                        if let Some(ct) = value.get("eval_count").and_then(|v| v.as_u64()) {
                            completion_tokens = Some(ct as usize);
                        }
                    }
                }
            }

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: if content.is_empty() { None } else { Some(content) },
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });

        Ok(rx)
    }
}
