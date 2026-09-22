use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

struct ContextOverflowMockClient {
    call_count: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for ContextOverflowMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let count = self.call_count.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        if count == 0 {
            // First attempt: simulate 400 Context Overflow error from official API
            return Err("This model's maximum context length is 5000 tokens. However, your messages resulted in 7200 tokens.".to_string());
        }

        // Second attempt: auto-healed after compaction, successfully respond
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Token(
                    "Self-healed and responded successfully!".to_string(),
                )))
                .await;
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("Self-healed and responded successfully!".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(options.messages.len() * 10),
                    completion_tokens: Some(10),
                }))
                .await;
        });

        Ok(rx)
    }
}

#[tokio::test]
async fn test_context_overflow_fail_compact_auto_retry() {
    let mut config = AgentConfig::new("test-model");
    config.pruning.max_context_tokens = 128_000;

    let mock_client = Arc::new(ContextOverflowMockClient {
        call_count: AtomicUsize::new(0),
    });

    let agent = AgentLoop::new(config).with_custom_client(mock_client.clone());

    let handle = agent
        .start("Hello, will this auto-heal?", None)
        .expect("Agent start");

    let result = handle.join().await.expect("Join should succeed");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(
        result.final_content.unwrap(),
        "Self-healed and responded successfully!"
    );
    // Verify it attempted twice: 1st failed with 400, 2nd auto-healed and succeeded!
    assert_eq!(mock_client.call_count.load(Ordering::SeqCst), 2);
}
