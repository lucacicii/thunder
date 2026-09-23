use crate::adapters::ModelAdapter;
use crate::catalog::ModelSpec;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use tokio_util::sync::CancellationToken;

/// Adapter implementing Google Gemini's GenerateContent API
pub struct GoogleAdapter;

impl ModelAdapter for GoogleAdapter {
    fn name(&self) -> &'static str {
        "google-gemini"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let (system, contents) = split_google_messages(&options.messages);
        let mut payload = json!({
            "contents": contents,
            "generationConfig": {
                "temperature": options.temperature,
                "maxOutputTokens": options.max_tokens.unwrap_or(spec.max_tokens),
            }
        });

        if let Some(system_text) = system {
            payload["systemInstruction"] = json!({
                "parts": [{"text": system_text}]
            });
        }

        if !options.tools.is_empty() {
            payload["tools"] = json!([{
                "functionDeclarations": options.tools.iter().map(|t| json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters
                })).collect::<Vec<_>>()
            }]);
        }

        payload
    }
}

pub struct GoogleClient {
    http: reqwest::Client,
    spec: ModelSpec,
    adapter: GoogleAdapter,
}

impl GoogleClient {
    pub fn new(spec: ModelSpec) -> Self {
        Self {
            http: reqwest::Client::new(),
            spec,
            adapter: GoogleAdapter,
        }
    }
}

#[async_trait]
impl LLMClientTrait for GoogleClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let model = options.model.clone().unwrap_or_else(|| self.spec.id.clone());
        let base_url = if self.spec.base_url.trim().is_empty() {
            "https://generativelanguage.googleapis.com/v1beta".to_string()
        } else {
            self.spec.base_url.trim_end_matches('/').to_string()
        };

        let mut url = format!("{base_url}/models/{model}:streamGenerateContent?alt=sse");
        if let Some(key) = &self.spec.api_key {
            url.push_str("&key=");
            url.push_str(key);
        }

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
                    .send(Err(format!("Google API returned HTTP {status}: {body}")))
                    .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buf = String::new();
            let mut content = String::new();
            let mut tool_calls = Vec::new();

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
                        let Ok(value) = serde_json::from_str::<Value>(data) else { continue };

                        if let Some(parts) = value.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()) {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    content.push_str(text);
                                    let _ = tx.send(Ok(LLMStreamChunk::Token(text.to_string()))).await;
                                }
                                if let Some(fc) = part.get("functionCall") {
                                    let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("tool").to_string();
                                    let args = fc.get("args").map(|a| a.to_string()).unwrap_or_else(|| "{}".to_string());
                                    tool_calls.push(ToolCall::new_function(
                                        format!("call_{}", tool_calls.len() + 1),
                                        name,
                                        args,
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: if content.is_empty() { None } else { Some(content) },
                    tool_calls,
                    finish_reason: "stop".to_string(),
                    prompt_tokens: None,
                    completion_tokens: None,
                }))
                .await;
        });

        Ok(rx)
    }
}

pub fn split_google_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system = None;
    let mut contents = Vec::new();

    for message in messages {
        match message {
            ChatMessage::System { content, .. } => {
                system = Some(match system.take() {
                    Some(existing) => format!("{existing}\n\n{content}"),
                    None => content.clone(),
                });
            }
            ChatMessage::User { content, .. } => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": content }]
                }));
            }
            ChatMessage::Assistant { content, tool_calls, .. } => {
                let mut parts = Vec::new();
                if let Some(c) = content {
                    if !c.is_empty() {
                        parts.push(json!({ "text": c }));
                    }
                }
                if let Some(calls) = tool_calls {
                    for call in calls {
                        parts.push(json!({
                            "functionCall": {
                                "name": call.function.name,
                                "args": serde_json::from_str::<Value>(&call.function.arguments).unwrap_or(json!({}))
                            }
                        }));
                    }
                }
                contents.push(json!({
                    "role": "model",
                    "parts": parts
                }));
            }
            ChatMessage::Tool { tool_call_id, content, name } => {
                contents.push(json!({
                    "role": "function",
                    "parts": [{
                        "functionResponse": {
                            "name": name.clone().unwrap_or_else(|| tool_call_id.clone()),
                            "response": { "result": content }
                        }
                    }]
                }));
            }
        }
    }

    (system, contents)
}
