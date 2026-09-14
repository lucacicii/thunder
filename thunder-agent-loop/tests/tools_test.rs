use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::tools::builtin::bash::BashTool;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio_util::sync::CancellationToken;

struct MultiplierTool;

#[async_trait]
impl AgentTool for MultiplierTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "multiply",
            "Multiply two numbers",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let a = args.get("a").and_then(|v| v.as_f64()).ok_or("Missing 'a'")?;
        let b = args.get("b").and_then(|v| v.as_f64()).ok_or("Missing 'b'")?;
        Ok(json!({ "result": a * b }).to_string())
    }
}

#[tokio::test]
async fn test_tool_registry_and_executor() {
    let mut registry = ToolRegistry::new(1024, Duration::from_secs(5));
    registry.register(Arc::new(MultiplierTool));

    let executor = ToolExecutor::new(registry);
    let calls = vec![
        ToolCall::new_function("call_1", "multiply", "{\"a\": 6, \"b\": 7}"),
        ToolCall::new_function("call_2", "multiply", "{\"a\": 10, \"b\": 20}"),
    ];

    let results = executor
        .execute_all(&calls, 1, CancellationToken::new(), None)
        .await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].result.output, "{\"result\":42.0}");
    assert_eq!(results[1].result.output, "{\"result\":200.0}");
    assert!(!results[0].result.is_error);
    assert!(!results[1].result.is_error);
}

#[tokio::test]
async fn test_bash_tool_execution() {
    let bash = BashTool::default();
    let ctx = ToolExecutionContext {
        tool_call_id: "test_1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    };

    let res = bash
        .execute(json!({ "command": "echo 'thunder-agent-loop-test'" }), &ctx)
        .await;

    assert!(res.is_ok());
    assert_eq!(res.unwrap().trim(), "thunder-agent-loop-test");
}
