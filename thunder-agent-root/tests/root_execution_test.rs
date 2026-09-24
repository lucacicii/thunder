use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

struct TestMockClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for TestMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn == 0 {
                let text = "Calling bash tool to test root execution.\n";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_1",
                            "bash",
                            "{\"command\":\"echo \\\"root test passed\\\"\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(20),
                        cached_tokens: None,
                    }))
                    .await;
            } else {
                let text = "Bash executed successfully. Final answer.";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(15),
                        cached_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

#[tokio::test]
async fn test_thunder_root_end_to_end_execution() {
    let base_cfg = AgentConfig::new("gpt-4o").with_unlimited_turns();
    let conv_plugin = ConversationPlugin::with_memory_store();
    let store = conv_plugin.store();

    let root = ThunderRoot::new(base_cfg)
        .with_plugin(conv_plugin)
        .with_plugin(SkillsPlugin::default())
        .with_plugin(McpPlugin::default());

    let session_id = "test_root_sess_1".to_string();
    let custom_client: Arc<dyn LLMClientTrait> = Arc::new(TestMockClient {
        turn: AtomicUsize::new(0),
    });

    let options = RootRunOptions {
        session_id: Some(session_id.clone()),
        use_mock: true,
        custom_client: Some(custom_client),
        cancellation_token: None,
        forced_plugins: None,
        register_builtins: true,
        thinking_level: None,
        role: None,
        permission: thunder_agent_loop::types::config::Permission::default(),
        pause_gate: None,
    };

    let mut handle = root
        .execute("Use custom skill and check conversation history", options)
        .await
        .expect("Root execute should succeed");

    // Verify dynamic plugin selection triggered skills & conversation
    assert!(handle.selection.active_plugin_ids.contains(&"skills".to_string()));
    assert!(handle.selection.active_plugin_ids.contains(&"conversation".to_string()));

    let mut event_count = 0;
    if let Some(mut rx) = handle.take_events() {
        while let Some(_event) = rx.recv().await {
            event_count += 1;
        }
    }
    assert!(event_count > 0);

    let result = handle.join().await.expect("Join should succeed");
    assert_eq!(result.run_result.finish_reason, FinishReason::Done);
    assert_eq!(result.run_result.stats.total_turns, 2);
    assert_eq!(result.run_result.stats.total_tool_executions, 1);
    assert!(result.final_content.unwrap().contains("Bash executed successfully"));

    // Verify conversation was persisted by ConversationPlugin
    let loaded = store.load(&session_id).await.unwrap();
    assert!(loaded.is_some());
    let conv = loaded.unwrap();
    assert_eq!(conv.id, session_id);
    assert!(!conv.messages.is_empty());
}
