use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

struct TsPluginMockClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for TsPluginMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn == 0 {
                // First turn: LLM calls the dynamic TypeScript tool
                let text = "I will use the TypeScript helper tool.\n";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_ts_1",
                            "ts_echo_helper",
                            "{\"message\":\"hello from root agent\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(25),
                        cached_tokens: None,
                        reasoning_tokens: None,
                    }))
                    .await;
            } else {
                // Second turn: LLM acknowledges output
                let text = "TypeScript tool completed. Task finished.";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
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
async fn test_script_plugin_integrated_with_thunder_root_and_onion_middleware() {
    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();

    // Create a dynamic TypeScript plugin in the workspace
    let ts_content = r#"
export default definePlugin({
  name: "helper_plugin",
  version: "1.0.0",
  systemPrompt: async (ctx) => "System prompt from TypeScript plugin",
  tools: [
    {
      name: "ts_echo_helper",
      description: "Echo message and write to file via ctx.fs",
      execute: async (args, ctx) => {
        await ctx.fs.writeFile("out.txt", `Saved: ${args.message}`);
        return `Echo: ${args.message}`;
      }
    }
  ]
});
"#;
    tokio::fs::write(plugins_dir.join("echo_plugin.ts"), ts_content).await.unwrap();

    let script_plugin = ScriptPlugin::new().with_workspace(ws_dir.clone());

    let base_cfg = AgentConfig::new("mock/test-model".to_string()).with_max_turns(5);
    let root = ThunderRoot::new(base_cfg)
        .with_workspace(ws_dir.clone())
        .with_plugin(script_plugin);

    let client = Arc::new(TsPluginMockClient {
        turn: AtomicUsize::new(0),
    });

    let options = RootRunOptions {
        session_id: Some("test_ts_root_sess".to_string()),
        use_mock: false,
        custom_client: Some(client),
        cancellation_token: None,
        forced_plugins: Some(vec!["script_plugin".to_string()]),
        register_builtins: true,
        thinking_level: None,
        role: None,
        permission: thunder_agent_loop::types::config::Permission::default(),
        pause_gate: None,
    };

    let result = root
        .run("Run the TypeScript helper tool", options)
        .await
        .expect("Root execution should succeed");

    assert_eq!(result.run_result.finish_reason, FinishReason::Done);
    assert!(result.final_content.unwrap().contains("TypeScript tool completed"));

    // Verify file written via ctx.fs.writeFile exists and has correct content
    let out_file = ws_dir.join("out.txt");
    assert!(out_file.exists(), "out.txt should have been written via atomic transaction");
    let content = tokio::fs::read_to_string(&out_file).await.unwrap();
    assert_eq!(content, "Saved: hello from root agent");
}
