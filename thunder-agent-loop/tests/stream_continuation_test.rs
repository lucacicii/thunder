use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

struct DroppedStreamMockClient {
    call_count: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for DroppedStreamMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let count = self.call_count.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            if count == 0 {
                // Attempt 1: emit partial tokens, then drop connection mid-stream!
                let _ = tx
                    .send(Ok(LLMStreamChunk::Token(
                        "Part 1: The engine of Thunder ".to_string(),
                    )))
                    .await;
                let _ = tx
                    .send(Err(
                        "Network connection dropped: connection reset by peer".to_string()
                    ))
                    .await;
            } else {
                // Attempt 2: continuation resumed! Emit remainder and complete
                let _ = tx
                    .send(Ok(LLMStreamChunk::Token(
                        "Part 2: seamlessly auto-healed!".to_string(),
                    )))
                    .await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("Part 2: seamlessly auto-healed!".to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(15),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

#[tokio::test]
async fn test_stream_interruption_breakpoint_continuation_heals_successfully() {
    let mut config = AgentConfig::new("test-model");
    config.max_stream_retries = 2;

    let mock_client = Arc::new(DroppedStreamMockClient {
        call_count: AtomicUsize::new(0),
    });

    let agent = AgentLoop::new(config).with_custom_client(mock_client.clone());

    let handle = agent
        .start("Tell me about Thunder engine resilience", None)
        .expect("Agent start");

    let result = handle.join().await.expect("Join should succeed");

    assert_eq!(result.finish_reason, FinishReason::Done);
    // Verified that both parts were seamlessly merged!
    let final_content = result.final_content.expect("final content");
    assert!(final_content.contains("Part 1: The engine of Thunder "));
    assert!(final_content.contains("Part 2: seamlessly auto-healed!"));
    assert_eq!(mock_client.call_count.load(Ordering::SeqCst), 2);
}
