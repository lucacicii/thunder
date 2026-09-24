use async_trait::async_trait;
use std::time::Duration;
use thunder_agent_loop::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct TuiMockClient;

#[async_trait]
impl LLMClientTrait for TuiMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
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
            let response = if last_user.to_lowercase().contains("hello") || last_user.to_lowercase().contains("hi") {
                "Hello! 👋 I am Thunder Assistant running in ultra-fast Mock mode. How can I help you today?".to_string()
            } else {
                format!(
                    "Received your request: \"{}\". Thunder Agent executed this successfully in Mock mode.",
                    last_user
                )
            };

            // Stream word by word for realistic typing effect
            let words: Vec<&str> = response.split_inclusive(' ').collect();
            for word in words {
                tokio::time::sleep(Duration::from_millis(25)).await;
                if tx.send(Ok(LLMStreamChunk::Token(word.to_string()))).await.is_err() {
                    return;
                }
            }

            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(response),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(15),
                    completion_tokens: Some(25),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });

        Ok(rx)
    }
}
