use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::message::ToolCall;
use thunder_tui::app::ExecutionMode;
use thunder_tui::prelude::*;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_app_state_and_focus_cycle() {
    let mut app = App::new("gpt-4o", true);
    assert_eq!(app.mode, ViewMode::Chat);
    assert_eq!(app.focus, FocusPane::Input);

    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Chat);

    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Input);
}

#[tokio::test]
async fn test_app_execution_mode_cycling() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();
    assert_eq!(app.execution_mode, ExecutionMode::AutoRouter);

    // Ctrl+P cycles to SequentialPipeline
    app.handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.execution_mode, ExecutionMode::SequentialPipeline);

    // Ctrl+P cycles to ParallelCouncil
    app.handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.execution_mode, ExecutionMode::ParallelCouncil);

    // Ctrl+P cycles to SingleAgent
    app.handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.execution_mode, ExecutionMode::SingleAgent);

    // Ctrl+P cycles back to AutoRouter
    app.handle_key(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.execution_mode, ExecutionMode::AutoRouter);
}

#[tokio::test]
async fn test_app_streaming_event_ingestion() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();

    // 1. Simulate key input
    app.handle_key(
        KeyEvent::new(KeyCode::Char('H'), KeyModifiers::empty()),
        tx.clone(),
    );
    app.handle_key(
        KeyEvent::new(KeyCode::Char('i'), KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "Hi");

    // 2. Simulate streaming tokens from Agent
    app.handle_agent_event(ObservedEvent {
        agent_id: "agent_1".to_string(),
        event: AgentEvent::TurnStart {
            turn: 1,
            timestamp: 100,
        },
    });
    assert_eq!(app.agent_status, AgentStatus::Thinking);

    app.handle_agent_event(ObservedEvent {
        agent_id: "agent_1".to_string(),
        event: AgentEvent::TokenDelta {
            turn: 1,
            delta: "Hello ".to_string(),
        },
    });
    app.handle_agent_event(ObservedEvent {
        agent_id: "agent_1".to_string(),
        event: AgentEvent::TokenDelta {
            turn: 1,
            delta: "world!".to_string(),
        },
    });
    assert_eq!(app.agent_status, AgentStatus::Streaming);
    assert_eq!(app.streaming_delta, "Hello world!");

    // 3. Simulate tool call ready
    app.handle_agent_event(ObservedEvent {
        agent_id: "agent_1".to_string(),
        event: AgentEvent::ToolCallReady {
            turn: 1,
            tool_call: ToolCall::new_function("call_1", "read_file", "{\"path\":\"Cargo.toml\"}"),
        },
    });
    assert_eq!(app.active_tool_calls.len(), 1);
    assert_eq!(app.active_tool_calls[0].name, "read_file");
    assert!(matches!(app.conversation.messages.last(), Some(thunder_agent_loop::ChatMessage::Assistant { tool_calls: Some(_), .. })));

    // The tool result is committed immediately after the tool event, not appended at finalization.
    app.handle_agent_event(ObservedEvent {
        agent_id: "agent_1".to_string(),
        event: AgentEvent::ToolExecResult {
            turn: 1,
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            result: thunder_agent_loop::ToolExecutionResult {
                output: "file contents".to_string(),
                is_error: false,
                truncated: false,
                original_bytes: 13,
                duration_ms: 4,
            },
        },
    });
    assert!(app.active_tool_calls.is_empty());
    assert!(matches!(app.conversation.messages.last(), Some(thunder_agent_loop::ChatMessage::Tool { .. })));

    // 4. Finish
    app.handle_agent_finished("agent_1".to_string(), true, None);
    assert_eq!(app.agent_status, AgentStatus::Idle);
    assert_eq!(app.conversation.messages.len(), 4); // System + Assistant text + Tool Call + Tool Result
}

#[tokio::test]
async fn test_app_new_session_and_shortcuts() {
    let mut app = App::new("gpt-4o", true);
    let (tx, _rx) = mpsc::unbounded_channel();

    app.conversation.add_user_message("Old message");
    assert_eq!(app.conversation.messages.len(), 2); // System + User

    // Ctrl+N creates new session
    app.handle_key(
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.conversation.messages.len(), 1); // New session has System message

    // Ctrl+M toggles monitor
    app.handle_key(
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.mode, ViewMode::OrchestraMonitor);

    // Ctrl+M toggles back
    app.handle_key(
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.mode, ViewMode::Chat);
}

#[tokio::test]
async fn test_app_scrolling_and_mouse() {
    let mut app = App::new("gpt-4o", true);
    app.last_max_scroll = 50;
    app.auto_scroll = true;

    // 1. PageUp scrolls up by 10 lines
    app.scroll_up(10);
    assert_eq!(app.scroll_offset, 40);
    assert!(!app.auto_scroll);

    // 2. Home scrolls to top
    app.scroll_to_top();
    assert_eq!(app.scroll_offset, 0);

    // 3. Mouse ScrollDown scrolls down by 3 lines
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::empty(),
    });
    assert_eq!(app.scroll_offset, 3);

    // 4. End snaps to bottom
    app.scroll_to_bottom();
    assert_eq!(app.scroll_offset, 50);
    assert!(app.auto_scroll);
}
