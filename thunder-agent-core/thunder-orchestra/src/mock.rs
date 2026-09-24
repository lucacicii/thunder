use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use thunder_agent_loop::{
    ChatRequestOptions, LLMClientTrait, LLMStreamChunk, ToolCall,
};
use tokio_util::sync::CancellationToken;

/// Fixture LLM so B can demo without a live key.
///
/// Each constructed client is independent (one per A unit).
pub struct RoleMockClient {
    role: String,
    turn: AtomicUsize,
}

impl RoleMockClient {
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            turn: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LLMClientTrait for RoleMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        let role = self.role.clone();
        let last_user = options
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                thunder_agent_loop::ChatMessage::User { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();

        tokio::spawn(async move {
            if turn == 0 {
                let text = format!("[{role}] acknowledged: {last_user}\n");
                let _ = tx.send(Ok(LLMStreamChunk::Token(text.clone()))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{role}_1"),
                            "bash",
                            "{\"command\":\"echo orchestra-ok\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(8),
                        completion_tokens: Some(8),
                        cached_tokens: None,
                    }))
                    .await;
            } else {
                let text = format!("[{role}] done. Prior task was: {last_user}");
                let _ = tx.send(Ok(LLMStreamChunk::Token(text.clone()))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(12),
                        completion_tokens: Some(10),
                        cached_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}
