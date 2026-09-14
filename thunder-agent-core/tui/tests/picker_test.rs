use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use thunder_tui::app::{App, ExecutionMode};
use thunder_tui::picker::{PickerItem, PickerKind, PickerResult, PickerState};
use tokio::sync::mpsc;

#[test]
fn test_picker_state_navigation_and_filtering() {
    let mut picker = PickerState::new();

    let items = vec![
        PickerItem::new("gpt-4o", "GPT-4o", "OpenAI Flagship"),
        PickerItem::new("claude-3-7-sonnet", "Claude 3.7 Sonnet", "Anthropic Hybrid"),
        PickerItem::new("deepseek-chat", "DeepSeek V3", "DeepSeek Chat"),
    ];

    picker.open(PickerKind::SelectModel, None, items);
    assert!(picker.is_open);
    assert_eq!(picker.filtered_items().len(), 3);
    assert_eq!(picker.selected_index, 0);

    // Test Down arrow
    picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    assert_eq!(picker.selected_index, 1);
    assert_eq!(picker.selected_item().unwrap().id, "claude-3-7-sonnet");

    // Test Up arrow
    picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
    assert_eq!(picker.selected_index, 0);

    // Test filtering by typing 'deep'
    picker.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()));
    picker.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()));
    picker.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()));
    picker.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::empty()));

    assert_eq!(picker.filtered_items().len(), 1);
    assert_eq!(picker.selected_item().unwrap().id, "deepseek-chat");

    // Test Enter selection
    let res = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    match res {
        PickerResult::Selected(kind, item) => {
            assert_eq!(kind, PickerKind::SelectModel);
            assert_eq!(item.id, "deepseek-chat");
        }
        _ => panic!("Expected Selected result"),
    }
    assert!(!picker.is_open);
}

#[tokio::test]
async fn test_app_interactive_pickers_integration() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();

    // 1. /model with an explicit id still switches immediately
    app.execute_slash_command("/model anthropic/claude-3-7-sonnet-latest", tx.clone());
    assert_eq!(app.model.selection_id(), "anthropic/claude-3-7-sonnet-latest");

    // 2. Trigger /mode without args -> opens mode picker
    app.execute_slash_command("/mode", tx.clone());
    assert!(app.picker.is_open);
    assert_eq!(app.picker.kind, PickerKind::SelectMode);

    // Navigate to Pipeline
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()), tx.clone());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx.clone());
    assert_eq!(app.execution_mode, ExecutionMode::SequentialPipeline);
}

#[tokio::test]
async fn test_selecting_skill_attaches_handler_without_dumping_playbook() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();

    app.picker.open(
        PickerKind::SelectSkill,
        None,
        vec![PickerItem::new(
            "code-review",
            "code-review",
            "Systematic code quality and security review",
        )],
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx);

    let handle = app.active_skill.expect("skill handler should be attached");
    assert_eq!(handle.name, "code-review");
    assert!(!handle.system_prompt_fragment().contains("## Review Instructions"));

    let dumped = app.conversation.messages.iter().any(|m| match m {
        thunder_agent_loop::types::message::ChatMessage::Assistant { content: Some(c), .. } => {
            c.contains("## Review Instructions") || c.contains("prompt_instructions")
        }
        _ => false,
    });
    assert!(!dumped, "full skill playbook must not be dumped into TUI chat");
}
