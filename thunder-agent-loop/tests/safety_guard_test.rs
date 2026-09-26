use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::pruning::strategy::ContextPruner;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::tools::builtin::bash::BashTool;
use thunder_agent_loop::tools::sanitizer::{
    is_binary_data, sanitize_tool_output, strip_ansi_escapes,
};
use thunder_agent_loop::types::config::{AgentConfig, ContextPruningConfig};
use thunder_agent_loop::types::event::FinishReason;
use thunder_agent_loop::types::message::{ChatMessage, Role, ToolCall};
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use thunder_agent_loop::AgentLoop;
use tokio_util::sync::CancellationToken;

#[test]
fn test_ansi_and_binary_sanitizer() {
    let colored_log = "\x1b[32m[INFO]\x1b[0m Starting server on \x1b[1;34mport 8080\x1b[0m...";
    assert_eq!(
        strip_ansi_escapes(colored_log),
        "[INFO] Starting server on port 8080..."
    );

    let raw_elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(is_binary_data(&raw_elf));

    let sanitized = sanitize_tool_output(String::from_utf8_lossy(&raw_elf).to_string());
    assert!(sanitized.contains("[Binary data omitted:"));
}

#[tokio::test]
async fn test_bash_non_interactive_environment() {
    let bash = BashTool::default();
    let ctx = ToolExecutionContext {
        tool_call_id: "test_env".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    };

    let res = bash
        .execute(
            json!({ "command": "echo \"CI=$CI,TERM=$TERM,DEBIAN=$DEBIAN_FRONTEND\"" }),
            &ctx,
        )
        .await;

    assert!(res.is_ok());
    let output = res.unwrap();
    assert!(output.contains("CI=true"));
    assert!(output.contains("TERM=dumb"));
    assert!(output.contains("DEBIAN=noninteractive"));
}

#[test]
fn test_atomic_turn_group_pruning_no_orphans() {
    let mut ctx = ContextBuffer::new();
    ctx.set_system_prompt("System prompt");

    // Add 5 turns, each with Assistant(tool_calls) + 2 Tool responses
    for i in 1..=5 {
        ctx.push(ChatMessage::user(format!("Question {}", i)));
        ctx.push(ChatMessage::assistant(
            Some(format!("Thinking {}", i)),
            Some(vec![
                ToolCall::new_function(format!("call_{}_a", i), "tool_a", "{}"),
                ToolCall::new_function(format!("call_{}_b", i), "tool_b", "{}"),
            ]),
        ));
        ctx.push(ChatMessage::tool(format!("call_{}_a", i), "Output A", None));
        ctx.push(ChatMessage::tool(format!("call_{}_b", i), "Output B", None));
    }

    let pruner = ContextPruner::new(ContextPruningConfig {
        reserve_tokens: 32,
        keep_recent_tokens: 40,
        summarizer_model: None,
        summarizer_max_tokens: 4096,
        max_context_tokens: 220, // Force the emergency trim to run
        pin_system_prompt: true,
    });

    let removed = pruner.emergency_trim(&mut ctx);
    assert!(removed > 0, "emergency trim must drop old turns");
    assert!(
        ctx.estimated_tokens() <= 220,
        "emergency trim must respect the hard window"
    );

    // Verify context validity: System prompt must be index 0
    let messages = ctx.get_messages();
    assert_eq!(messages[0].role(), Role::System);

    // Verify that every Tool message is preceded by an Assistant message with matching tool_call_id
    let mut pending_ids = std::collections::HashSet::new();
    for msg in &messages {
        match msg {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for c in calls {
                    pending_ids.insert(c.id.clone());
                }
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                assert!(
                    pending_ids.remove(tool_call_id),
                    "Found orphaned Tool message with ID '{}'!",
                    tool_call_id
                );
            }
            _ => {}
        }
    }
}

// Mock LLM that repeats the exact same tool call
struct RepetitiveToolLLMClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for RepetitiveToolLLMClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn < 3 {
                // Repeat exact same tool call 3 times
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("Checking...".to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_1",
                            "read_file",
                            "{\"path\":\"same.txt\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(10),
                        completion_tokens: Some(10),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            } else {
                // Conclude on turn 4
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("Saw the system warning, concluding task.".to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(20),
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

struct DummyReadFileTool;

#[async_trait]
impl AgentTool for DummyReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "read_file",
            "Read file",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        Ok("file content constant".to_string())
    }
}

#[tokio::test]
async fn test_repetition_warning_injected() {
    let config = AgentConfig::new("test-model");
    let mock_client = Arc::new(RepetitiveToolLLMClient {
        turn: AtomicUsize::new(0),
    });

    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(DummyReadFileTool));

    let result = agent.run("Check same file repeatedly", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 4);

    // Verify that the 3rd tool output contains the repetition notice
    let messages = result.messages;
    let third_tool_msg = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .nth(2)
        .unwrap();
    if let ChatMessage::Tool { content, .. } = third_tool_msg {
        assert!(content.contains("System Notice: You have executed the exact same tool"));
    }
}
