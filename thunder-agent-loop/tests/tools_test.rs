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

    // Test with cwd
    let res_cwd = bash
        .execute(json!({ "command": "pwd", "cwd": "/" }), &ctx)
        .await;
    assert!(res_cwd.is_ok());
    assert_eq!(res_cwd.unwrap().trim(), "/");

    // Test with timeout_ms
    let res_timeout = bash
        .execute(json!({ "command": "sleep 5", "timeout_ms": 100 }), &ctx)
        .await;
    assert!(res_timeout.is_err());
    assert!(res_timeout.unwrap_err().contains("timeout"));
}

#[tokio::test]
async fn test_agent_loop_with_builtins_builder() {
    let cfg = thunder_agent_loop::types::config::AgentConfig::new("mock-model");
    let agent = thunder_agent_loop::AgentLoop::new(cfg).with_builtins();
    // AgentLoop successfully built with bash, read_file, and write_file
    assert_eq!(agent.status(), thunder_agent_loop::core::state::LoopStatus::Idle);
}

#[tokio::test]
async fn test_turn_off_transactions_without_modifying_code() {
    // Option 1: Using AgentConfig builder
    let cfg1 = thunder_agent_loop::types::config::AgentConfig::new("mock-model")
        .without_transactions();
    assert!(!cfg1.middleware.enable_transaction);

    // Option 2: Using direct boolean toggle in config
    let mut cfg2 = thunder_agent_loop::types::config::AgentConfig::new("mock-model");
    cfg2.middleware.enable_transaction = false;
    assert!(!cfg2.middleware.enable_transaction);

    // Option 3: Using AgentLoop fluent method
    let agent = thunder_agent_loop::AgentLoop::new(cfg2).without_transactions();
    assert!(!agent.config().middleware.enable_transaction);

    // Option 4: Disabling all middlewares for bare-metal execution
    let cfg_bare = thunder_agent_loop::types::config::AgentConfig::new("mock-model")
        .without_middlewares();
    assert!(!cfg_bare.middleware.enable_transaction);
    assert!(!cfg_bare.middleware.enable_security_guard);
    assert!(!cfg_bare.middleware.enable_resource_guard);
    assert!(!cfg_bare.middleware.enable_output_post_processor);
}
