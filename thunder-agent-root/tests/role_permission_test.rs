//! End-to-end proof that a role's permission tier gates which built-in tools
//! the host registers — i.e. the model never even sees a denied tool.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::types::config::Permission;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

/// Records the tool names advertised on the *first* turn, then finishes cleanly.
struct ToolRecordingClient {
    turn: AtomicUsize,
    seen_tools: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl LLMClientTrait for ToolRecordingClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        let seen = Arc::clone(&self.seen_tools);

        tokio::spawn(async move {
            if turn == 0 {
                let mut names: Vec<String> = options
                    .tools
                    .iter()
                    .map(|t| t.function.name.clone())
                    .collect();
                names.sort();
                seen.lock().await.extend(names);
            }
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });

        Ok(rx)
    }
}

async fn tools_for(permission: Permission) -> Vec<String> {
    let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let client: Arc<dyn LLMClientTrait> = Arc::new(ToolRecordingClient {
        turn: AtomicUsize::new(0),
        seen_tools: Arc::clone(&seen),
    });

    let root = ThunderRoot::new(AgentConfig::new("gpt-4o").with_unlimited_turns());

    let options = RootRunOptions {
        session_id: Some(format!("role_perm_{}", permission.as_str())),
        use_mock: true,
        custom_client: Some(client),
        cancellation_token: None,
        forced_plugins: Some(vec![]),
        register_builtins: true,
        thinking_level: None,
        role: None,
        permission,
        pause_gate: None,
    };

    let handle = root.execute("probe tools", options).await.expect("root execute");
    handle.join().await.expect("join");

    let out = seen.lock().await.clone();
    out
}

#[tokio::test]
async fn read_role_exposes_only_read_file() {
    let tools = tools_for(Permission::Read).await;
    assert!(tools.contains(&"read_file".to_string()), "read_file present: {tools:?}");
    assert!(!tools.contains(&"write_file".to_string()), "write_file must be absent: {tools:?}");
    assert!(!tools.contains(&"bash".to_string()), "bash must be absent: {tools:?}");
}

#[tokio::test]
async fn write_role_adds_write_file_but_not_bash() {
    let tools = tools_for(Permission::Write).await;
    assert!(tools.contains(&"read_file".to_string()), "{tools:?}");
    assert!(tools.contains(&"write_file".to_string()), "{tools:?}");
    assert!(!tools.contains(&"bash".to_string()), "bash must be absent: {tools:?}");
}

#[tokio::test]
async fn bash_role_exposes_all_three() {
    let tools = tools_for(Permission::Bash).await;
    for expected in ["read_file", "write_file", "bash"] {
        assert!(tools.contains(&expected.to_string()), "{expected} missing: {tools:?}");
    }
}
