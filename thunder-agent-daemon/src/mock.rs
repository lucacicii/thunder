use async_trait::async_trait;
use std::time::Duration;
use thunder_agent_loop::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use tokio_util::sync::CancellationToken;

#[derive(Default, Clone)]
pub struct DaemonMockClient;

#[async_trait]
impl LLMClientTrait for DaemonMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let last_user = options
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                thunder_agent_loop::ChatMessage::User { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "hello".to_string());

        tokio::spawn(async move {
            let response = if last_user.to_lowercase().contains("hello")
                || last_user.to_lowercase().contains("hi")
            {
                "Hello! ⚡ I am Thunder Agent Daemon running in Sidecar mode. I am ready to assist you in Electron!".to_string()
            } else {
                format!(
                    "Received prompt: \"{}\". Thunder Daemon successfully executed this in Mock mode.",
                    last_user
                )
            };

            let words: Vec<&str> = response.split_inclusive(' ').collect();
            for word in words {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Ok(LLMStreamChunk::Completed {
                            content: None,
                            tool_calls: vec![],
                            finish_reason: "cancelled".to_string(),
                            prompt_tokens: None,
                            completion_tokens: None,
                            cached_tokens: None,
                            reasoning_tokens: None,
                        })).await;
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(30)) => {
                        if tx.send(Ok(LLMStreamChunk::Token(word.to_string()))).await.is_err() {
                            return;
                        }
                    }
                }
            }

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(response),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: None,
                    completion_tokens: None,
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });

        Ok(rx)
    }
}
