//! TUI run-flow tests driven through the injected client factory seam.
//!
//! These are the tests the removed runtime mock mode used to make possible:
//! instead of shipping a mock client, the app accepts a `ClientFactory`, so
//! tests can drive a full submit → stream → finish cycle deterministically.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_tui::app::{App, ClientFactory, ExecutionMode};
use thunder_tui::event::AppEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const FAKE_REPLY: &str = "hello from the injected fake client";

struct FakeClient;

#[async_trait]
impl LLMClientTrait for FakeClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Token(FAKE_REPLY.to_string())))
                .await;
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(FAKE_REPLY.to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(5),
                    completion_tokens: Some(6),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn fake_factory() -> ClientFactory {
    Arc::new(|_cfg: &AgentConfig| Some(Arc::new(FakeClient) as Arc<dyn LLMClientTrait>))
}

/// Drive one prompt to completion, feeding the terminal events back into the
/// app the same way `TuiRunner` does, and return the final assistant text.
async fn drive_one_prompt(app: &mut App, prompt: &str) -> Option<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    app.submit_prompt(prompt.to_string(), tx);

    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = rx.recv().await {
            match event {
                AppEvent::Agent(observed) => app.handle_agent_event(observed),
                AppEvent::AgentFinished {
                    agent_id,
                    success,
                    final_text,
                    authoritative_messages,
                    raw_messages,
                } => {
                    app.handle_agent_finished(
                        agent_id,
                        success,
                        final_text.clone(),
                        authoritative_messages,
                        raw_messages,
                    );
                    return (success, final_text);
                }
                _ => {}
            }
        }
        (false, None)
    })
    .await
    .expect("run should finish within the timeout");

    assert!(finished.0, "run must succeed: {:?}", finished.1);
    finished.1
}

#[tokio::test]
async fn plugin_host_mode_uses_injected_client_factory() {
    let mut app = App::new("fake-model").with_client_factory(fake_factory());
    assert_eq!(app.execution_mode, ExecutionMode::AutoRouter);

    drive_one_prompt(&mut app, "say hello").await;

    // The projection replaced the conversation with the run's messages.
    let last = app.conversation.messages.last().expect("assistant message");
    assert_eq!(last.content_str(), Some(FAKE_REPLY));
}

#[tokio::test]
async fn single_agent_mode_uses_injected_client_factory() {
    let mut app = App::new("fake-model").with_client_factory(fake_factory());
    app.execution_mode = ExecutionMode::SingleAgent;

    drive_one_prompt(&mut app, "say hello").await;

    let last = app.conversation.messages.last().expect("assistant message");
    assert_eq!(last.content_str(), Some(FAKE_REPLY));
}
