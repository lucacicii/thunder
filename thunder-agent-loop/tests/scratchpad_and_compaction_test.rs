use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
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

// Mock LLM that runs 20 turns generating tools and large outputs
struct LongHorizonMockLLM {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for LongHorizonMockLLM {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn < 18 {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(format!("Executing milestone exploration turn {}...", turn + 1)),
                        tool_calls: vec![ToolCall::new_function(
                            format!("call_{}", turn),
                            "big_output",
                            format!("{{\"step\":{}}}", turn), // Unique arguments per turn
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(15),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("Long horizon task finalized after multiple compactions.".to_string()),
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

#[tokio::test]
async fn test_long_horizon_rolling_compaction_with_artifacts() {
    let mut config = AgentConfig::new("test-model");
    config.pruning = ContextPruningConfig {
        max_context_tokens: 1500, // Balanced budget allowing turns to build before rolling compaction
        tool_eviction_threshold_tokens: 1200,
        preserve_last_turns: 3,
        pin_system_prompt: true,
        strategy: PruningStrategy::Hybrid,
    };
    config.scratchpad = ScratchpadConfig {
        threshold_bytes: 500, // persist outputs > 500 bytes
        preview_bytes: 100,
        auto_cleanup: true,
        base_dir: std::env::temp_dir().join(format!("thunder_comp_test_{}", std::process::id())),
    };

    let mock_client = Arc::new(LongHorizonMockLLM {
        turn: AtomicUsize::new(0),
    });

    let mut agent = AgentLoop::new(config).with_custom_client(mock_client);
    agent.register_tool(Arc::new(BigOutputTool));

    let result = agent.run("Start 20-turn long horizon mission", None).await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(result.stats.total_turns, 19);

    // Verify that index 0 is System Prompt and index 1 is Milestone State Digest
    assert_eq!(result.messages[0].role(), Role::System);
    assert_eq!(result.messages[1].role(), Role::System);
    let digest_content = result.messages[1].content_str().unwrap();
    assert!(digest_content.contains("【Previous Conversation Summary & Milestone State Digest】"));
    assert!(digest_content.contains("Persisted Artifacts Available"));
}
