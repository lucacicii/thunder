use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

struct DirectAnswerLLMClient;

#[async_trait]
impl LLMClientTrait for DirectAnswerLLMClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("I have all the information directly: The answer is 42.".to_string()),
                    tool_calls: vec![], // No tools requested!
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(12),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

struct MultiTurnAutonomousLLMClient {
    turn: AtomicUsize,
    conclude_at_turn: usize,
}

#[async_trait]
impl LLMClientTrait for MultiTurnAutonomousLLMClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let current_turn = self.turn.fetch_add(1, Ordering::SeqCst);

        let is_final = current_turn >= self.conclude_at_turn;

        tokio::spawn(async move {
            if !is_final {
                // LLM decides it needs more information -> issues tool call
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(format!("I need more info for step {}...", current_turn + 1)),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{}", current_turn),
                            "fetch_info",
                            format!("{{\"query\":\"step_{}\"}}", current_turn + 1),
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(15),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            } else {
                // LLM decides information is sufficient -> no tool calls, conclude task!
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("All information gathered. Task completed successfully!".to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(20),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

struct MockInfoTool;

#[async_trait]
impl AgentTool for MockInfoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "fetch_info",
            "Fetch information",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        Ok(format!("Result for query: {}", q))
    }
}

#[tokio::test]
async fn test_llm_decides_immediate_stop_without_tools() {
    let config = AgentConfig::new("test-model"); // Unlimited turns by default
    let mock_client = Arc::new(DirectAnswerLLMClient);

    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(MockInfoTool));

    let result = agent.run("What is 6 * 7?", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 1); // Exact 1 turn!
    assert_eq!(result.stats.total_tool_executions, 0); // 0 tools called
    assert!(result.final_content.unwrap().contains("42"));
}

#[tokio::test]
async fn test_llm_autonomously_decides_when_info_is_sufficient() {
    let config = AgentConfig::new("test-model");
    // LLM calls tools for 3 turns, and concludes on turn 4 (index 3)
    let mock_client = Arc::new(MultiTurnAutonomousLLMClient {
        turn: AtomicUsize::new(0),
        conclude_at_turn: 3,
    });

    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(MockInfoTool));

    let result = agent.run("Solve multi-step research problem", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 4); // 3 tool turns + 1 final conclusion turn
    assert_eq!(result.stats.total_tool_executions, 3); // exactly 3 tool queries
    assert!(result.final_content.unwrap().contains("Task completed successfully!"));
}
