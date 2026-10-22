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
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // 5. Load saved session via `/load sess-saved-42`
    assert!(app.handle_slash_command("/load sess-saved-42").await);
    assert_eq!(app.conversation.id, "sess-saved-42");
    assert!(app
        .timeline
        .iter()
        .any(|i| matches!(i, TimelineItem::User { text } if text == "Hello from saved session")));

    // 6. Test /stats command
    assert!(app.handle_slash_command("/stats").await);
    assert!(app.timeline.iter().any(
        |i| matches!(i, TimelineItem::SystemNotice { text } if text.contains("Session Statistics"))
    ));

    // 7. Test /think command
    assert!(app.handle_slash_command("/think high").await);
    assert_eq!(app.thinking_level.as_deref(), Some("high"));

    // 8. Test /rename command
    assert!(app.handle_slash_command("/rename My Great Task").await);
    assert_eq!(app.conversation.title.as_deref(), Some("My Great Task"));

    // 9. Test /compact command
    assert!(app.handle_slash_command("/compact").await);
    assert!(app
        .timeline
        .iter()
        .any(|i| matches!(i, TimelineItem::SystemNotice { text } if text.contains("compacted"))));

    // 10. Test /workspace command
    assert!(app.handle_slash_command("/workspace").await);

    // 11. Test /resume 1 (numeric index resume)
    assert!(app.handle_slash_command("/resume 1").await);
    assert_eq!(app.conversation.id, "sess-saved-42");

    // 12. Test /export command
    let export_target = store_dir.path().join("exported_session.md");
    assert!(
        app.handle_slash_command(&format!("/export {}", export_target.display()))
            .await
    );
    assert!(export_target.exists());

    // 13. Test /prompt command with template
    let prompts_dir = app.workspace_dir.join(".thunder").join("prompts");
    tokio::fs::create_dir_all(&prompts_dir).await.unwrap();
    tokio::fs::write(prompts_dir.join("review.md"), "Please review code.")
        .await
        .unwrap();

    assert!(app.handle_slash_command("/prompt").await);
    assert!(app.handle_slash_command("/prompt review").await);
    assert_eq!(app.input, "Please review code.");

    // 14. Test /health command
    assert!(app.handle_slash_command("/health").await);
    assert!(app
        .timeline
        .iter()
        .any(|i| matches!(i, TimelineItem::SystemNotice { text } if text.contains("Thunder System Health"))));

    // 15. Test /queue command
    assert!(app.handle_slash_command("/queue").await);
    assert!(app.handle_slash_command("/queue clear").await);
}

#[tokio::test]
async fn picker_modal_and_autocomplete_popover_work_end_to_end() {
    use thunder_tui::{filter_slash_commands, PickerItem, PickerKind};

    // 1. Test slash commands autocomplete filter
    let m_cmds = filter_slash_commands("/m");
    let names: Vec<&str> = m_cmds.iter().map(|c| c.name).collect();
    assert!(names.contains(&"model"));
    assert!(names.contains(&"mode"));

    let r_cmds = filter_slash_commands("/res");
    assert_eq!(r_cmds[0].name, "resume");

    let store_dir = tempdir().unwrap();
    let store = FsConversationStore::new(store_dir.path()).await.unwrap();
    let mut app = App::new("gpt-4o").with_store(store);

    // Populate a test item into model picker
    app.picker.open(
        PickerKind::Model,
        "Select LLM Model",
        vec![
            PickerItem::new("lucastech/zai/glm-5.3", "lucastech/zai/glm-5.3", "Zai GLM")
                .with_badge("1M ctx"),
            PickerItem::new(
                "lucastech/group/deepseek",
                "lucastech/group/deepseek",
                "DeepSeek Chat",
            )
            .with_badge("1M ctx"),
            PickerItem::new("openai/gpt-4o", "openai/gpt-4o", "GPT-4o").with_badge("128K ctx"),
        ],
    );
    assert!(app.picker.is_open);
    assert_eq!(app.picker.filtered_items().len(), 3);

    // Test typing filter in modal
    app.picker.handle_char('d');
    app.picker.handle_char('e');
    app.picker.handle_char('e');
    app.picker.handle_char('p');
    assert_eq!(app.picker.filtered_items().len(), 1);
    assert_eq!(
        app.picker.filtered_items()[0].id,
        "lucastech/group/deepseek"
    );

    // Test backspace in modal filter
    app.picker.handle_backspace();
    app.picker.handle_backspace();
    app.picker.handle_backspace();
    app.picker.handle_backspace();
    assert_eq!(app.picker.filtered_items().len(), 3);

    // Test navigation in modal
    app.picker.move_selection_down();
    assert_eq!(app.picker.selected_index, 1);
    assert_eq!(
        app.picker.selected_item().unwrap().id,
        "lucastech/group/deepseek"
    );

    // Test rendering frame with modal open
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // Commit selection in modal
    let selected = app.commit_picker_selection().await;
    assert_eq!(selected.as_deref(), Some("lucastech/group/deepseek"));
    assert!(!app.picker.is_open);
    assert_eq!(app.model, "lucastech/group/deepseek");

    // Test Popover rendering when input starts with '/'
    app.input = "/mod".to_string();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // Test /tools opens tools picker modal
    assert!(app.handle_slash_command("/tools").await);
    assert!(app.picker.is_open);
    assert_eq!(app.picker.kind, Some(PickerKind::Tools));
    app.picker.close();

    // Test /model opens model picker modal
    assert!(app.handle_slash_command("/model").await);
    assert!(app.picker.is_open);
    assert_eq!(app.picker.kind, Some(PickerKind::Model));
    app.picker.close();

    // 16. Test argument completion context for /think and /mode
    use thunder_tui::{resolve_completion_context, InputCompletionContext};

    let think_ctx = resolve_completion_context("/think ", &app).expect("arg context for think");
    match think_ctx {
        InputCompletionContext::Argument {
            command,
            candidates,
            ..
        } => {
            assert_eq!(command.name, "think");
            assert!(candidates.contains(&"off".to_string()));
            assert!(candidates.contains(&"high".to_string()));
            assert!(candidates.contains(&"max".to_string()));
        }
        _ => panic!("expected Argument context"),
    }

    let think_filtered =
        resolve_completion_context("/think h", &app).expect("filtered arg context");
    match think_filtered {
        InputCompletionContext::Argument { candidates, .. } => {
            assert_eq!(candidates, vec!["high".to_string()]);
        }
        _ => panic!("expected Argument context"),
    }

    // Test /think with no args cycles thinking effort directly without opening a modal!
    app.thinking_level = Some("off".to_string());
    assert!(app.handle_slash_command("/think").await);
    assert!(
        !app.picker.is_open,
        "Type B command /think must NEVER open a modal"
    );
    assert_eq!(app.thinking_level.as_deref(), Some("low"));

    // Render frame with Popover showing /think options
    app.input = "/think ".to_string();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // 17. Test suggestions and ghost text placeholder
    app.input.clear();
    app.update_suggestions();
    assert!(
        !app.suggestions.is_empty(),
        "must produce follow-up suggestions"
    );
    let _initial_sug = app.current_suggestion().unwrap().to_string();

    // Render frame with Ghost Text suggestion line
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // Cycle suggestion
    app.cycle_suggestion();

    // Accept suggestion with Tab
    assert!(app.accept_suggestion());
    assert!(!app.input.is_empty());
}
