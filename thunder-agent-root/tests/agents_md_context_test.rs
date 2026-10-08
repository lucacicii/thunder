use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;

struct PromptCapturingClient {
    seen: Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl LLMClientTrait for PromptCapturingClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let system = options
            .messages
            .first()
            .and_then(|m| match m {
                ChatMessage::System { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();
        *self.seen.lock().unwrap() = Some(system);

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: None,
                    completion_tokens: None,
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn test_thunder_root_loads_workspace_agents_md() {
    let ws = tempdir().unwrap();
    let agents_md_path = ws.path().join("AGENTS.md");
    tokio::fs::write(&agents_md_path, "Master instruction: build high performance system.").await.unwrap();

    let root = ThunderRoot::new(AgentConfig::new("test/model")).with_workspace(ws.path().to_path_buf());
    let client = Arc::new(PromptCapturingClient {
        seen: Mutex::new(None),
    });

    let opts = RootRunOptions {
        custom_client: Some(client.clone()),
        ..Default::default()
    };

    let handle = root.execute("Hello", opts).await.unwrap();
    let _ = handle.join().await;

    let sys_prompt = client.seen.lock().unwrap().clone().unwrap_or_default();
    assert!(
        sys_prompt.contains("Master instruction: build high performance system."),
        "System prompt should include workspace AGENTS.md content, got:\n{}",
        sys_prompt
    );
    assert!(
        sys_prompt.contains("Context Instructions"),
        "System prompt should contain Context Instructions header"
    );
}
