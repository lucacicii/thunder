use thunder_agent_loop::types::event::{AgentStats, FinishReason};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::AgentRunResult;
use thunder_conversation::prelude::*;

#[tokio::test]
async fn test_conversation_memory_crud() {
    let store = MemoryConversationStore::new();
    let manager = ConversationManager::new(store);

    let conv = manager
        .create_with_prompt(
            "conv_001",
            Some("My First Chat".to_string()),
            Some("You are a helpful assistant.".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(conv.id, "conv_001");
    assert_eq!(conv.title.as_deref(), Some("My First Chat"));
    assert_eq!(conv.messages.len(), 1);
    assert_eq!(conv.messages[0].role(), thunder_agent_loop::Role::System);

    // Append user message
    let updated = manager
        .append_user_message("conv_001", "Hello assistant!")
        .await
        .unwrap();
    assert_eq!(updated.messages.len(), 2);
    assert_eq!(updated.stats.turn_count, 1);
    assert!(updated.stats.total_tokens > 0);

    // Ingest mock agent run result
    let mock_result = AgentRunResult {
        agent_id: "test_agent".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("Hello! How can I help you today?".to_string()),
        stats: AgentStats {
            total_turns: 1,
            total_duration_ms: 150,
            total_tool_executions: 0,
            ..Default::default()
        },
        messages: vec![
            ChatMessage::user("Hello assistant!"),
            ChatMessage::assistant(Some("Hello! How can I help you today?".to_string()), None),
        ],
        raw_messages: None,
    };

    let with_agent = manager
        .append_run_result("conv_001", &mock_result)
        .await
        .unwrap();
    assert_eq!(with_agent.messages.len(), 4);

    // List and filter
    let list = manager.list(&ConversationFilter::new()).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "conv_001");

    // Fork
    let forked = manager
        .fork("conv_001", "conv_001_fork", Some(1))
        .await
        .unwrap();
    assert_eq!(forked.id, "conv_001_fork");
    assert_eq!(forked.parent_id.as_deref(), Some("conv_001"));

    // Archive
    let archived = manager.archive("conv_001").await.unwrap();
    assert_eq!(archived.status, ConversationStatus::Archived);

    // Delete
    let deleted = manager.delete("conv_001").await.unwrap();
    assert!(deleted);
    assert!(manager.get("conv_001").await.unwrap().is_none());
}

#[tokio::test]
async fn test_conversation_fs_store() {
    let tmp_dir = std::env::temp_dir().join(format!("thunder_conv_test_{}", now_ms()));
    let store = FsConversationStore::new(&tmp_dir).await.unwrap();
    let manager = ConversationManager::new(store);

    let mut conv = manager.create("fs_conv_1").await.unwrap();
    conv.add_user_message("Write a rust function");
    conv.add_assistant_message(
        Some("Let me calculate that".to_string()),
        Some(vec![ToolCall::new_function(
            "call_1",
            "calc",
            "{\"expr\":\"1+1\"}",
        )]),
    );
    conv.add_tool_message("call_1", "2", Some("calc".to_string()));
    conv.add_assistant_message(Some("The result is 2.".to_string()), None);
    manager.save(&conv).await.unwrap();

    // Reload from disk
    let loaded = manager
        .get("fs_conv_1")
        .await
        .unwrap()
        .expect("should load");
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.stats.tool_calls_count, 1);
    assert_eq!(loaded.stats.turn_count, 1);

    // Turns extraction
    let turns = extract_turns(&loaded.messages);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].tool_calls_count(), 1);
    assert_eq!(turns[0].final_content.as_deref(), Some("The result is 2."));

    // Markdown export
    let md = ConversationExporter::to_markdown(&loaded);
    assert!(md.contains("Write a rust function"));
    assert!(md.contains("Tool Call"));
    assert!(md.contains("The result is 2."));

    // Truncate turns
    let truncated = truncate_turns(&loaded, 1);
    assert_eq!(truncated.len(), 4);

    // Cleanup
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
}

#[tokio::test]
async fn test_bridge_context_input() {
    let mut conv = Conversation::new("bridge_conv");
    conv.add_user_message("What is 42?");
    conv.add_assistant_message(Some("The answer to life.".to_string()), None);

    let input = conv.as_context_input();
    match input {
        thunder_agent_loop::loop_engine::engine::ContextInput::Messages(msgs) => {
            assert_eq!(msgs.len(), 2);
        }
        _ => panic!("Expected ContextInput::Messages"),
    }

    let buf = conv.build_context_buffer();
    assert_eq!(buf.len(), 2);
    assert!(buf.estimated_tokens() > 0);
}

#[tokio::test]
async fn raw_transcript_sidecar_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsConversationStore::new(dir.path()).await.unwrap();

    let raw = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("do the thing"),
        ChatMessage::assistant_text("working"),
        ChatMessage::tool("call_1", "output", Some("bash".to_string())),
    ];

    let path = store.save_raw_transcript("sess_raw", &raw).await.unwrap();
    assert!(path.exists(), "sidecar file must be written");
    assert_eq!(path.file_name().unwrap(), "raw_transcript.jsonl");

    let loaded = store
        .load_raw_transcript("sess_raw")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.len(), raw.len());
    assert_eq!(loaded[1].content_str(), Some("do the thing"));

    // Overwrite is idempotent (atomic tmp+rename, not append-duplicating).
    store.save_raw_transcript("sess_raw", &raw).await.unwrap();
    let loaded_again = store
        .load_raw_transcript("sess_raw")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded_again.len(), raw.len(), "no duplicate accumulation");

    // Missing session → None, not an error.
    assert!(store.load_raw_transcript("nope").await.unwrap().is_none());
}

/// Two `FsConversationStore` instances over one root model the real deployment:
/// the TUI and the daemon both default to `~/.thunder/conversations` and hold
/// independent in-memory indexes. A save by one process must never erase the
/// other's rows from the shared `index.json`.
#[tokio::test]
async fn fs_store_shared_root_saves_do_not_clobber_each_other() {
    let dir = tempfile::tempdir().unwrap();

    // Both "processes" open before any conversation exists: neither knows
    // about the other's future writes.
    let store_a = FsConversationStore::new(dir.path()).await.unwrap();
    let store_b = FsConversationStore::new(dir.path()).await.unwrap();

    let conv_a = Conversation::new("shared_a");
    store_a.save(&conv_a).await.unwrap();

    // B's in-memory index predates A's save; B saving must merge, not
    // overwrite the on-disk index with its (conv_a-less) map.
    let conv_b = Conversation::new("shared_b");
    store_b.save(&conv_b).await.unwrap();

    let listed_by_a = store_a.list(&ConversationFilter::new()).await.unwrap();
    let listed_by_b = store_b.list(&ConversationFilter::new()).await.unwrap();
    let ids = |rows: Vec<ConversationSummary>| {
        let mut v: Vec<String> = rows.into_iter().map(|r| r.id).collect();
        v.sort();
        v
    };
    assert_eq!(
        ids(listed_by_a),
        vec!["shared_a".to_string(), "shared_b".to_string()],
        "store A must still see its own conversation after B's save"
    );
    assert_eq!(
        ids(listed_by_b),
        vec!["shared_a".to_string(), "shared_b".to_string()],
        "store B must see A's conversation without a restart (merged read)"
    );
}

/// A delete issued by one process must survive the other process's next
/// write, even though the other process still holds the deleted row in its
/// stale in-memory index.
#[tokio::test]
async fn fs_store_shared_root_delete_is_not_resurrected() {
    let dir = tempfile::tempdir().unwrap();

    let store_a = FsConversationStore::new(dir.path()).await.unwrap();
    let store_b = FsConversationStore::new(dir.path()).await.unwrap();

    let conv = Conversation::new("doomed");
    store_a.save(&conv).await.unwrap();
    // B loads the row into its in-memory index (same as a daemon that listed
    // conversations at startup).
    let _ = store_b.list(&ConversationFilter::new()).await.unwrap();

    // A deletes: directory removed first, then the index row.
    assert!(store_a.delete("doomed").await.unwrap());

    // B still carries "doomed" in memory and saves an unrelated conversation.
    // The persist path must prune the vanished directory's row instead of
    // resurrecting it in the shared index.
    let unrelated = Conversation::new("unrelated");
    store_b.save(&unrelated).await.unwrap();

    let rows = store_a.list(&ConversationFilter::new()).await.unwrap();
    let ids: Vec<String> = rows.into_iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        vec!["unrelated".to_string()],
        "a delete issued by one process must not be undone by another's save"
    );
}
