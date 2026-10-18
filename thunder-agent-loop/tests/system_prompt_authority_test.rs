//! The host re-assembles `config.system_prompt` for every run (base prompt,
//! AGENTS.md/CLAUDE.md, plugin contributions, skills catalog, `.thunder`
//! prompt file). A transcript handed back through `ContextInput::Messages`
//! carries the *previous* run's system message; it must not outrank the
//! freshly assembled one.

use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::loop_engine::engine::{AgentLoop, ContextInput};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::message::ChatMessage;
use tokio_util::sync::CancellationToken;

struct OneShotClient;

#[async_trait]
impl LLMClientTrait for OneShotClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("ok".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn stale_transcript() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("STALE_PROMPT_FROM_FIRST_RUN"),
        ChatMessage::user("hello"),
    ]
}

async fn run_with(input: ContextInput) -> Vec<ChatMessage> {
    let config = AgentConfig::new("test-model").with_system_prompt("FRESH_HOST_ASSEMBLED_PROMPT");
    let agent = AgentLoop::new(config).with_custom_client(Arc::new(OneShotClient));
    agent.run(input, None).await.unwrap().messages
}

#[tokio::test]
async fn messages_input_replaces_a_stale_system_message() {
    let messages = run_with(ContextInput::Messages(stale_transcript())).await;

    assert_eq!(
        messages[0].role(),
        thunder_agent_loop::types::message::Role::System
    );
    assert_eq!(
        messages[0].content_str(),
        Some("FRESH_HOST_ASSEMBLED_PROMPT"),
        "the host-assembled prompt must win over the transcript's copy"
    );
    // The transcript itself is preserved below the system message.
    assert_eq!(messages.len(), 3, "system + user + assistant");
    assert_eq!(messages[1].content_str(), Some("hello"));
}

#[tokio::test]
async fn messages_input_inserts_the_prompt_when_none_is_carried() {
    let messages = run_with(ContextInput::Messages(vec![ChatMessage::user("hello")])).await;

    assert_eq!(
        messages[0].content_str(),
        Some("FRESH_HOST_ASSEMBLED_PROMPT"),
        "a transcript with no system message still gets the host prompt"
    );
    assert_eq!(messages[1].content_str(), Some("hello"));
}

#[tokio::test]
async fn buffer_input_also_yields_to_the_host_prompt() {
    let mut ctx = ContextBuffer::with_messages(stale_transcript());
    ctx.push(ChatMessage::user("second"));
    let messages = run_with(ContextInput::Buffer(ctx)).await;

    assert_eq!(
        messages[0].content_str(),
        Some("FRESH_HOST_ASSEMBLED_PROMPT"),
        "a pre-built buffer must not freeze the first run's prompt"
    );
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn a_run_without_a_host_prompt_keeps_the_transcript_system_message() {
    let config = AgentConfig::new("test-model");
    let agent = AgentLoop::new(config).with_custom_client(Arc::new(OneShotClient));
    let messages = agent
        .run(ContextInput::Messages(stale_transcript()), None)
        .await
        .unwrap()
        .messages;

    assert_eq!(
        messages[0].content_str(),
        Some("STALE_PROMPT_FROM_FIRST_RUN")
    );
}
