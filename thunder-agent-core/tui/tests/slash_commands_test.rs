use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::tempdir;
use thunder_conversation::prelude::*;
use thunder_tui::app::{App, ExecutionMode};
use thunder_tui::commands::filter_commands;
use tokio::sync::mpsc;

#[test]
fn test_slash_command_autocomplete_filtering() {
    // 1. Slash prefix returns all commands
    let all = filter_commands("/");
    assert_eq!(all.len(), 16);

    // 2. Filter by prefix
    let sk = filter_commands("/sk");
    assert!(!sk.is_empty());
    assert_eq!(sk[0].name, "skills");

    let res = filter_commands("/re");
    let res_names: Vec<_> = res.iter().map(|c| c.name).collect();
    assert!(res_names.contains(&"resume"));

    let m = filter_commands("/m");
    let names: Vec<_> = m.iter().map(|c| c.name).collect();
    assert!(names.contains(&"model"));
    assert!(names.contains(&"mode"));
    assert!(names.contains(&"mcp"));

    // 3. No slash returns empty
    let non = filter_commands("hello");
    assert!(non.is_empty());
}

#[tokio::test]
async fn test_slash_command_enter_and_tab_autocomplete() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();

    // 1. Typing '/' and pressing Enter should autocomplete to the first command (e.g. /resume )
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty()), tx.clone());
    assert_eq!(app.input, "/");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx.clone());
    assert_eq!(app.input, "/resume ");

    // 2. Typing '/sk' and pressing Tab should autocomplete to '/skills '
    app.input.clear();
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()), tx.clone());
    assert_eq!(app.input, "/sk");

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()), tx.clone());
    assert_eq!(app.input, "/skills ");

    // 3. Typing '/re' and pressing Down arrow then Enter should select
    app.input.clear();
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx.clone());
    assert_eq!(app.input, "/resume ");
}

#[tokio::test]
async fn test_slash_commands_execution_flow() {
    let tmp_dir = tempdir().unwrap();
    let fs_store = FsConversationStore::new(tmp_dir.path()).await.unwrap();

    let mut app = App::new("gpt-4o", true).with_store(fs_store);
    let (tx, _rx) = mpsc::unbounded_channel();

    // 1. Test /help
    let handled = app.execute_slash_command("/help", tx.clone());
    assert!(handled);
    let last_msg = app.conversation.messages.last().unwrap();
    let content = match last_msg {
        thunder_agent_loop::types::message::ChatMessage::Assistant { content, .. } => content.clone().unwrap(),
        _ => panic!("Expected assistant message"),
    };
    assert!(content.contains("Thunder TUI Slash Commands Reference"));

    // 2. Test /resume (opens interactive session picker)
    app.session_list = vec![ConversationSummary {
        id: "sess_test_1".to_string(),
        title: Some("Test Session".to_string()),
        parent_id: None,
        status: ConversationStatus::Active,
        message_count: 3,
        turn_count: 1,
        total_tokens: 100,
        tags: vec![],
        created_at_ms: 1000,
        updated_at_ms: 1000,
    }];
    app.execute_slash_command("/resume", tx.clone());
    assert!(app.picker.is_open);
    assert_eq!(app.picker.kind, thunder_tui::picker::PickerKind::ResumeSession);
    app.picker.close();

    // 3. Test /model
    app.execute_slash_command("/model claude-3-7-sonnet", tx.clone());
    assert_eq!(app.model.selection_id(), "openai/claude-3-7-sonnet");

    // 4. Test /mode
    app.execute_slash_command("/mode pipeline", tx.clone());
    assert_eq!(app.execution_mode, ExecutionMode::SequentialPipeline);

    // 5. Test /config
    app.execute_slash_command("/config temperature 0.8", tx.clone());
    assert_eq!(app.temperature, 0.8);

    app.execute_slash_command("/config max_turns 50", tx.clone());
    assert_eq!(app.max_turns, 50);

    // 6. Test /stats
    app.execute_slash_command("/stats", tx.clone());
    let stats_msg = app.conversation.messages.last().unwrap();
    let stats_text = match stats_msg {
        thunder_agent_loop::types::message::ChatMessage::Assistant { content, .. } => content.clone().unwrap(),
        _ => panic!("Expected assistant message"),
    };
    assert!(stats_text.contains("Session Statistics"));

    // 7. Test /export
    let export_file = tmp_dir.path().join("exported_session.md");
    let export_cmd = format!("/export {}", export_file.display());
    app.execute_slash_command(&export_cmd, tx.clone());
    assert!(export_file.exists());
    let exported_content = std::fs::read_to_string(&export_file).unwrap();
    assert!(exported_content.contains("- **ID**:") || exported_content.contains("Dialogue Flow"));

    // 8. Test /clear
    app.execute_slash_command("/clear", tx.clone());
    assert_eq!(app.conversation.messages.len(), 2); // System prompt + Welcome message

    // 9. Test /quit
    app.execute_slash_command("/quit", tx);
    assert!(app.should_quit);
}
