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
    let forked = manager.fork("conv_001", "conv_001_fork", Some(1)).await.unwrap();
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
        Some(vec![ToolCall::new_function("call_1", "calc", "{\"expr\":\"1+1\"}")]),
    );
    conv.add_tool_message("call_1", "2", Some("calc".to_string()));
    conv.add_assistant_message(Some("The result is 2.".to_string()), None);
    manager.save(&conv).await.unwrap();

    // Reload from disk
    let loaded = manager.get("fs_conv_1").await.unwrap().expect("should load");
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
