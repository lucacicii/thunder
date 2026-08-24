use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio_util::sync::CancellationToken;

struct MockLLMClient {
    turn_counter: AtomicUsize,
    stop_after_turns: usize,
}

impl MockLLMClient {
    fn new(stop_after_turns: usize) -> Self {
        Self {
            turn_counter: AtomicUsize::new(0),
            stop_after_turns,
        }
    }
}

#[async_trait]
impl LLMClientTrait for MockLLMClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let count = self.turn_counter.fetch_add(1, Ordering::SeqCst);
        let stop_threshold = self.stop_after_turns;

        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                let _ = tx.send(Err("Cancelled".to_string())).await;
                return;
            }

            if count < stop_threshold {
                // Emit progressive tool call with advancing argument
                let _ = tx.send(Ok(LLMStreamChunk::Token(format!("Step {}...", count)))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(format!("Step {}...", count)),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{}", count),
                            "add",
                            format!("{{\"x\":{},\"y\":1}}", count),
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(10),
                        completion_tokens: Some(8),
                    }))
                    .await;
            } else {
                // Final response
                let _ = tx.send(Ok(LLMStreamChunk::Token(" Done at last.".to_string()))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(" Done at last.".to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(25),
                        completion_tokens: Some(12),
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

struct AddTool;

#[async_trait]
impl AgentTool for AddTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "add",
            "Add two numbers",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["x", "y"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let x = args.get("x").and_then(|v| v.as_f64()).ok_or("Missing x")?;
        let y = args.get("y").and_then(|v| v.as_f64()).ok_or("Missing y")?;
        Ok(json!({ "sum": x + y }).to_string())
    }
}

#[tokio::test]
async fn test_agent_loop_full_lifecycle() {
    let config = AgentConfig::new("test-model")
        .with_system_prompt("You are a test assistant")
        .with_max_turns(5);

    let mock_client = Arc::new(MockLLMClient::new(1));
    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(AddTool));

    let mut event_sub = agent.subscribe_events();
    let result = agent.run("Please add 15 and 25", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 2);
    assert_eq!(result.stats.total_tool_executions, 1);

    // Verify messages list
    assert_eq!(result.messages.len(), 5); // System, User, Assistant(tool_call), Tool(result), Assistant(final)

    // Drain events to verify TurnStart / TurnEnd / LoopComplete events
    let mut event_types = Vec::new();
    while let Ok(observed) = event_sub.try_recv() {
        match observed.event {
            thunder_agent_loop::types::event::AgentEvent::TurnStart { .. } => event_types.push("turn_start"),
            thunder_agent_loop::types::event::AgentEvent::TokenDelta { .. } => event_types.push("token_delta"),
            thunder_agent_loop::types::event::AgentEvent::ToolExecResult { .. } => event_types.push("tool_exec_result"),
            thunder_agent_loop::types::event::AgentEvent::TurnEnd { .. } => event_types.push("turn_end"),
            thunder_agent_loop::types::event::AgentEvent::LoopComplete { .. } => event_types.push("loop_complete"),
            _ => {}
        }
    }

    assert!(event_types.contains(&"turn_start"));
    assert!(event_types.contains(&"tool_exec_result"));
    assert!(event_types.contains(&"loop_complete"));
}

#[tokio::test]
async fn test_agent_loop_unlimited_turns() {
    // Default config has NO max turns (unlimited)
    let config = AgentConfig::new("test-model")
        .with_unlimited_turns();

    assert!(config.max_turns.is_none());

    // Mock client executes 12 progressive tool turns before finishing
    let mock_client = Arc::new(MockLLMClient::new(12));
    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(AddTool));

    let result = agent.run("Execute multi-step long task", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 13); // 12 tool turns + 1 final turn
    assert_eq!(result.stats.total_tool_executions, 12);
}

#[tokio::test]
async fn test_agent_loop_mid_stream_cancellation_classified_correctly() {
    struct SlowHangingClient;

    #[async_trait]
    impl LLMClientTrait for SlowHangingClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                // Emit one token and then wait for cancel
                let _ = tx.send(Ok(LLMStreamChunk::Token("Processing...".to_string()))).await;
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Err("Stream aborted by cancellation".to_string())).await;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                }
            });
            Ok(rx)
        }
    }

    let config = AgentConfig::new("test-model");
    let agent = AgentLoop::new(config).with_custom_client(Arc::new(SlowHangingClient));

    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Trigger cancellation after 20ms
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token_clone.cancel();
    });

    let result = agent.run("Cancel this task mid stream", Some(token)).await.unwrap();

    // Verify it is classified as Cancelled and NOT Error
    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert_eq!(result.final_content, None);
}

#[tokio::test]
async fn test_hard_repetition_limit_trips_circuit_breaker() {
    struct InfiniteRepeatingClient;

    #[async_trait]
    impl LLMClientTrait for InfiniteRepeatingClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            _cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("Calling same tool forever...".to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_repeat",
                            "add",
                            "{\"x\":1,\"y\":1}", // Exactly identical args every time
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(10),
                        completion_tokens: Some(10),
                    }))
                    .await;
            });
            Ok(rx)
        }
    }

    let config = AgentConfig::new("test-model");
    let mut agent = AgentLoop::new(config).with_custom_client(Arc::new(InfiniteRepeatingClient));
    agent.register_tool(Arc::new(AddTool));

    let result = agent.run("Test repetition guard", None).await.unwrap();

    // Guard terminates the loop with Error to prevent infinite token burn
    assert_eq!(result.finish_reason, FinishReason::Error);
    // Non-success exits do not keep a partial tool-turn mutter as final_content
    assert_eq!(result.final_content, None);
}

#[tokio::test]
async fn test_reasoning_only_final_turn_captured_in_final_content() {
    struct ReasoningModelClient;

    #[async_trait]
    impl LLMClientTrait for ReasoningModelClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            _cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                // Model outputs pure reasoning in final turn (content is empty)
                let _ = tx.send(Ok(LLMStreamChunk::ReasoningToken("Detailed analysis and conclusion...".to_string()))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: None, // No content
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(20),
                        completion_tokens: Some(15),
                    }))
                    .await;
            });
            Ok(rx)
        }
    }

    let config = AgentConfig::new("deepseek-reasoner");
    let agent = AgentLoop::new(config).with_custom_client(Arc::new(ReasoningModelClient));

    let result = agent.run("Review architecture", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    // User-visible result falls back to reasoning when the answer stream is empty.
    assert_eq!(
        result.final_content.as_deref(),
        Some("Detailed analysis and conclusion...")
    );
    // Transcript stays clean: reasoning is not impersonating assistant answer content.
    let last_assistant = result
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m, ChatMessage::Assistant { .. }))
        .expect("expected a final assistant message");
    assert_eq!(last_assistant.content_str(), None);
}

#[tokio::test]
async fn test_reasoning_on_tool_turn_is_not_written_into_assistant_content() {
    struct ReasoningThenToolClient {
        turn_counter: AtomicUsize,
    }

    #[async_trait]
    impl LLMClientTrait for ReasoningThenToolClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            _cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let turn = self.turn_counter.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                if turn == 0 {
                    // Reasoner thinks, then issues a tool call with no answer content.
                    let _ = tx
                        .send(Ok(LLMStreamChunk::ReasoningToken(
                            "I should add the numbers first.".to_string(),
                        )))
                        .await;
                    let _ = tx
                        .send(Ok(LLMStreamChunk::Completed {
                            content: None,
                            tool_calls: vec![ToolCall::new_function(
                                "call_0",
                                "add",
                                "{\"x\":1,\"y\":2}",
                            )],
                            finish_reason: "tool_calls".to_string(),
                            prompt_tokens: Some(10),
                            completion_tokens: Some(8),
                        }))
                        .await;
                } else {
                    let _ = tx.send(Ok(LLMStreamChunk::Token("Sum is 3.".to_string()))).await;
                    let _ = tx
                        .send(Ok(LLMStreamChunk::Completed {
                            content: None,
                            tool_calls: vec![],
                            finish_reason: "stop".to_string(),
                            prompt_tokens: Some(20),
                            completion_tokens: Some(6),
                        }))
                        .await;
                }
            });

            Ok(rx)
        }
    }

    let config = AgentConfig::new("deepseek-reasoner");
    let mock_client = Arc::new(ReasoningThenToolClient {
        turn_counter: AtomicUsize::new(0),
    });
    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(AddTool));

    let result = agent.run("Add 1 and 2", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.final_content.as_deref(), Some("Sum is 3."));

    let tool_turn_assistant = result
        .messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Assistant { tool_calls: Some(_), .. }))
        .expect("expected an assistant tool-call message");
    assert_eq!(
        tool_turn_assistant.content_str(),
        None,
        "reasoning must not be replayed as assistant content on a tool-call turn"
    );
    assert_ne!(
        tool_turn_assistant.content_str(),
        Some("I should add the numbers first.")
    );
}

#[tokio::test]
async fn test_max_turns_exit_does_not_keep_partial_final_content() {
    let config = AgentConfig::new("test-model").with_max_turns(1);
    let mock_client = Arc::new(MockLLMClient::new(5));
    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(AddTool));

    let result = agent.run("Keep calling tools", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::MaxTurnsExceeded);
    assert_eq!(result.stats.total_turns, 1);
    assert_eq!(result.final_content, None);
}
