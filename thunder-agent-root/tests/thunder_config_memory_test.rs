//! Integration coverage for the `.thunder` config layer and the memory plugin.
//!
//! These pin the behaviours that are easy to regress silently: layered config
//! merging, the appended (never replacing) system-prompt file, memory file
//! discovery, `@import` expansion, and the import jail.

#![cfg(feature = "conversation")]

use std::sync::{Arc, Mutex};
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use thunder_agent_root::thunder_config::{AgentSection, ThunderConfig};
use thunder_conversation::prelude::MemoryConversationStore;

fn root_with_ws(ws: &std::path::Path) -> ThunderRoot {
    StandardHostBuilder::new(Arc::new(MemoryConversationStore::new())).build(
        ThunderRoot::new(thunder_agent_loop::prelude::AgentConfig::new("test/model"))
            .with_workspace(ws.to_path_buf()),
    )
}

#[test]
fn memory_plugin_is_part_of_the_baseline_set() {
    let root = root_with_ws(std::path::Path::new("."));
    let ids: Vec<String> = root
        .registry()
        .list_manifests()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert!(
        ids.contains(&"memory".to_string()),
        "memory plugin missing from baseline: {ids:?}"
    );
}

#[tokio::test]
async fn memory_files_are_rendered_into_the_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let dir = ws.join(".thunder");
    std::fs::create_dir_all(dir.join("memory")).unwrap();
    std::fs::write(dir.join("THUNDER.md"), "# Project\n\nUse tabs, not spaces.\n").unwrap();
    std::fs::write(dir.join("memory").join("build.md"), "Build with `./test.sh`.\n").unwrap();
    std::fs::write(
        dir.join("THUNDER.local.md"),
        "Local: my editor is vim.\n",
    )
    .unwrap();

    let root = root_with_ws(ws);
    let handle = root
        .execute("hello", RootRunOptions::default())
        .await
        .unwrap();

    let prompt = handle
        .active_set
        .build_combined_system_prompt(Some("BASE"));
    assert!(
        prompt.contains("Use tabs, not spaces."),
        "project memory missing:\n{prompt}"
    );
    assert!(
        prompt.contains("Build with `./test.sh`."),
        "topic memory missing:\n{prompt}"
    );
    assert!(
        prompt.contains("Local: my editor is vim."),
        "local memory missing:\n{prompt}"
    );
    // The base prompt must be preserved — memory is appended, never a replacement.
    assert!(prompt.starts_with("BASE"));
}

#[tokio::test]
async fn memory_import_is_jailed_to_the_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let dir = ws.join(".thunder");

    // A secret outside the workspace that the import must never reach.
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "TOP_SECRET_PAYLOAD").unwrap();

    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("THUNDER.md"),
        format!("# Notes\n\n@/{}\n", secret.display().to_string().trim_start_matches('/')),
    )
    .unwrap();

    let root = root_with_ws(ws);
    let handle = root
        .execute("hi", RootRunOptions::default())
        .await
        .unwrap();
    let prompt = handle.active_set.build_combined_system_prompt(Some("BASE"));
    assert!(
        !prompt.contains("TOP_SECRET_PAYLOAD"),
        "import escaped the jail:\n{prompt}"
    );
}

/// A client that records the system prompt it was handed, so a test can assert
/// on the fully assembled prompt (base + config file + plugin contributions)
/// rather than only the plugin-contributed slice.
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
async fn config_system_prompt_file_is_appended() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let dir = ws.join(".thunder");
    std::fs::create_dir_all(dir.join("prompts")).unwrap();
    std::fs::write(
        dir.join("prompts").join("system.md"),
        "PROJECT_RULE: always run fmt before commit.",
    )
    .unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"version":1,"agent":{"systemPromptFile":"prompts/system.md"}}"#,
    )
    .unwrap();

    let root = root_with_ws(ws);
    let client = Arc::new(PromptCapturingClient {
        seen: Mutex::new(None),
    });
    let handle = root
        .execute(
            "go",
            RootRunOptions {
                custom_client: Some(client.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let _ = handle.join().await;

    let prompt = client.seen.lock().unwrap().clone().unwrap_or_default();
    assert!(
        prompt.contains("PROJECT_RULE: always run fmt before commit."),
        "appended prompt file missing in assembled system prompt:\n{prompt}"
    );
    // The base autonomous prompt must still be present: the file is appended.
    assert!(
        prompt.contains("intent first, proceed step by step"),
        "base prompt was replaced instead of appended:\n{prompt}"
    );
}

#[test]
fn config_merge_is_layered_by_precedence() {
    // user → project → local, where local wins.
    let mut cfg = ThunderConfig {
        agent: AgentSection {
            max_turns: Some(5),
            ..Default::default()
        },
        ..Default::default()
    };
    // A later layer only declaring thinking_level must not clear max_turns.
    let overlay = ThunderConfig {
        agent: AgentSection {
            thinking_level: Some("high".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    cfg.merge(overlay);
    assert_eq!(cfg.agent.max_turns, Some(5));
    assert_eq!(cfg.agent.thinking_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn malformed_config_does_not_abort_the_run() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let dir = ws.join(".thunder");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), "{ this is not json").unwrap();

    // Loading must degrade to defaults rather than erroring.
    let cfg = ThunderConfig::load(Some(ws)).await;
    assert_eq!(cfg.agent.max_turns, None);
}
