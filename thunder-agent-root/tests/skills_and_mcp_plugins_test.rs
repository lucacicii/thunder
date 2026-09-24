use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

struct SkillsAndMcpMockClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for SkillsAndMcpMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn == 0 {
                // First turn: call load_skill tool
                let text = "I will load the skill to see instructions.\n";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_skill_1",
                            "load_skill",
                            "{\"skill_name\":\"git-master\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(25),
                        cached_tokens: None,
                    }))
                    .await;
            } else if turn == 1 {
                // Second turn: call MCP tool
                let text = "Skill loaded. Now invoking MCP tool.\n";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_mcp_1",
                            "mcp_remote_mock_fetch_data",
                            "{\"query\":\"git status\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(25),
                        cached_tokens: None,
                    }))
                    .await;
            } else {
                // Final turn
                let text = "Executed skill and MCP tool successfully. All tasks complete.";
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(options.messages.len() * 10),
                        completion_tokens: Some(20),
                        cached_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

#[tokio::test]
async fn test_thunder_root_skills_and_mcp_tool_execution() {
    let base_cfg = AgentConfig::new("gpt-4o").with_unlimited_turns();

    // 1. Setup SkillsPlugin with a skill
    let skill = Skill::new(
        "git-master",
        "Git atomic commit and rebase operations",
        "Use atomic commits and write descriptive commit messages.",
    )
    .with_trigger("git");

    let skills_plugin = SkillsPlugin::new().with_skill(skill);

    // 2. Setup McpPlugin with a mock client
    let mock_transport = Arc::new(MockTransport::new());
    let mcp_client = McpClient::from_transport("remote", mock_transport);
    mcp_client
        .initialize(InitializeParams::default())
        .await
        .expect("Mcp client init");

    let mcp_plugin = McpPlugin::new().with_client(mcp_client).await;

    // 3. Assemble ThunderRoot
    let root = ThunderRoot::new(base_cfg)
        .with_plugin(skills_plugin)
        .with_plugin(mcp_plugin);

    let session_id = "test_skills_mcp_sess_1".to_string();
    let custom_client: Arc<dyn LLMClientTrait> = Arc::new(SkillsAndMcpMockClient {
        turn: AtomicUsize::new(0),
    });

    let options = RootRunOptions {
        session_id: Some(session_id.clone()),
        use_mock: true,
        custom_client: Some(custom_client),
        cancellation_token: None,
        forced_plugins: Some(vec!["skills".to_string(), "mcp".to_string()]),
        register_builtins: true,
        thinking_level: None,
    };

    let result = root
        .run("Run git-master skill and call remote MCP tool", options)
        .await
        .expect("Root execution should succeed");

    assert_eq!(result.run_result.finish_reason, FinishReason::Done);
    assert_eq!(result.run_result.stats.total_turns, 3);
    assert_eq!(result.run_result.stats.total_tool_executions, 2);
    assert!(result.final_content.unwrap().contains("Executed skill and MCP tool successfully"));
}
