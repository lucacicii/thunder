use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::registry::ToolRegistry;
use tokio_util::sync::CancellationToken;

struct BigOutputTool;

#[async_trait]
impl AgentTool for BigOutputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("big_output", "Produces 100KB of log output", json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let mut out = String::new();
        for i in 1..=1000 {
            if i == 500 {
                out.push_str(&format!("Line {:04}: CRITICAL_KEY_VALUE_42981\n", i));
            } else {
                out.push_str(&format!("Line {:04}: Normal background execution log message\n", i));
            }
        }
        Ok(out)
    }
}

#[tokio::test]
async fn test_scratchpad_large_output_and_lossless_retrieval() {
    let temp_dir = std::env::temp_dir().join(format!("thunder_test_scratch_{}", std::process::id()));
    let config = ScratchpadConfig {
        base_dir: temp_dir.clone(),
        threshold_bytes: 1024, // 1KB threshold
        preview_bytes: 200,
        auto_cleanup: false, // keep for inspection
    };

    let manager = ScratchpadManager::new("sess_test_lossless", config);
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5))
        .with_scratchpad(manager.clone());

    registry.register(Arc::new(BigOutputTool));

    let exec_res = registry
        .execute_tool_call(
            &ToolCall::new_function("call_1", "big_output", "{}"),
            1,
            CancellationToken::new(),
            None,
        )
        .await;

    assert!(!exec_res.is_error);
    assert!(exec_res.output.contains("[Large Output Saved to Disk]"));
    assert!(exec_res.output.contains("File Path:"));

    let manifest = manager.get_manifest();
    assert_eq!(manifest.artifacts.len(), 1);
    let artifact = &manifest.artifacts[0];
    assert!(artifact.file_path.exists());

    // Use ReadFileTool to retrieve line 500 from the persisted artifact on disk
    let read_tool = ReadFileTool::default();
    let read_ctx = ToolExecutionContext {
        tool_call_id: "read_1".to_string(),
        turn: 2,
        cancellation_token: CancellationToken::new(),
    };

    let retrieved = read_tool
        .execute(
            json!({
                "path": artifact.file_path.to_str().unwrap(),
                "offset": 500,
                "limit": 1
            }),
            &read_ctx,
        )
        .await
        .unwrap();

    assert!(retrieved.contains("CRITICAL_KEY_VALUE_42981"));

    // Cleanup
    manager.cleanup().await.unwrap();
    assert!(!manager.session_dir().exists());
}
