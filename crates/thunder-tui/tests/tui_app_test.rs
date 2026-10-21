use ratatui::{backend::TestBackend, Terminal};
use serde_json::json;
use tempfile::tempdir;
use thunder_core::{Conversation, FsConversationStore, SteerQueues};
use thunder_protocol::{AgentEvent, AgentStats, FinishReason, ToolExecResultPayload};
use thunder_tui::{ui, App, TimelineItem, ToolCardStatus};

#[tokio::test]
async fn tui_app_handles_slash_commands_events_and_renders_cleanly() {
    let store_dir = tempdir().unwrap();
    let store = FsConversationStore::new(store_dir.path()).await.unwrap();

    // Pre-populate a saved conversation for `/load`
    let mut saved = Conversation::new("sess-saved-42").with_model("gpt-4o");
    saved.add_user_message("Hello from saved session");
    saved.add_assistant_turn(Some("Saved reply".into()), None, None, None);
    thunder_core::ConversationStore::save(&store, &saved)
        .await
        .unwrap();

    let mut app = App::new("lucastech/zai/glm-5.3").with_store(store);

    // 1. Mode toggle via `/mode`
    assert_eq!(app.mode, "yolo");
    assert!(app.handle_slash_command("/mode plan").await);
    assert_eq!(app.mode, "plan");

    // 2. Streaming agent events update timeline items
    app.apply_agent_event(AgentEvent::TurnStart {
        turn: 1,
        timestamp: 100,
        estimated_prompt_tokens: 512,
    });
    assert_eq!(app.estimated_tokens, 512);

    app.apply_agent_event(AgentEvent::ReasoningDelta {
        turn: 1,
        delta: "Analyzing file...".into(),
    });
    app.apply_agent_event(AgentEvent::TokenDelta {
        turn: 1,
        delta: "Here is the answer.".into(),
    });
    app.apply_agent_event(AgentEvent::ToolExecStart {
        turn: 1,
        tool_call_id: "tc-1".into(),
        name: "core__read_file".into(),
        arguments: json!({ "path": "Cargo.toml" }),
    });
    app.apply_agent_event(AgentEvent::ToolExecResult {
        turn: 1,
        tool_call_id: "tc-1".into(),
        name: "core__read_file".into(),
        result: ToolExecResultPayload {
            tool_call_id: "tc-1".into(),
            output: "[workspace]".into(),
            is_error: false,
            truncated: false,
            original_bytes: 11,
            duration_ms: 8,
        },
    });
    app.apply_agent_event(AgentEvent::LoopComplete {
        finish_reason: FinishReason::Done,
        final_content: Some("Here is the answer.".into()),
        stats: AgentStats {
            total_turns: 1,
            total_duration_ms: 42,
            ..Default::default()
        },
    });

    let has_completed_tool_card = app.timeline.iter().any(|item| {
        matches!(
            item,
            TimelineItem::ToolCallCard {
                name,
                status: ToolCardStatus::Success { duration_ms: 8, .. },
                ..
            } if name == "core__read_file"
        )
    });
    assert!(has_completed_tool_card);

    // 3. Test mid-run steering queue injection
    let queues = SteerQueues::new_shared();
    app.active_steer = Some(queues.clone());
    app.steer_active_run("Focus on tests".into());
    assert_eq!(queues.steering.len(), 1);

    // 4. Render frame onto Ratatui `TestBackend` to verify layout never panics
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &app)).unwrap();

    // 5. Load saved session via `/load sess-saved-42`
    assert!(app.handle_slash_command("/load sess-saved-42").await);
    assert_eq!(app.conversation.id, "sess-saved-42");
    assert!(app
        .timeline
        .iter()
        .any(|i| matches!(i, TimelineItem::User { text } if text == "Hello from saved session")));
}
