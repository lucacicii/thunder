use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::ToolCall;
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
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let count = self.turn_counter.fetch_add(1, Ordering::SeqCst);
        let stop_threshold = self.stop_after_turns;

        tokio::spawn(async move {
            if count < stop_threshold {
                // Emit tool call
                let _ = tx.send(Ok(LLMStreamChunk::Token(format!("Step {}...", count)))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(format!("Step {}...", count)),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{}", count),
                            "add",
                            "{\"x\":1,\"y\":1}",
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
    while let Ok(evt) = event_sub.try_recv() {
        match evt {
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

    // Mock client executes 12 continuous tool turns before finishing
    let mock_client = Arc::new(MockLLMClient::new(12));
    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(AddTool));

    let result = agent.run("Execute multi-step long task", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 13); // 12 tool turns + 1 final turn
    assert_eq!(result.stats.total_tool_executions, 12);
}
