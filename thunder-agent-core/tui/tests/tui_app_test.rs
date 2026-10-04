use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::message::ToolCall;
use thunder_tui::app::ExecutionMode;
use thunder_tui::prelude::*;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_app_state_and_focus_cycle() {
    let mut app = App::new("gpt-4o");
    assert_eq!(app.mode, ViewMode::Chat);
    assert_eq!(app.focus, FocusPane::Input);

    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Chat);

    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Input);
}

#[tokio::test]
async fn test_app_execution_mode_cycling() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    assert_eq!(app.execution_mode, ExecutionMode::AutoRouter);

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
    let mut app = App::new("gpt-4o");
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
    assert!(matches!(
        app.conversation.messages.last(),
        Some(thunder_agent_loop::ChatMessage::Assistant {
            tool_calls: Some(_),
            ..
        })
    ));

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
                telemetry: None,
            },
        },
    });
    assert!(app.active_tool_calls.is_empty());
    assert!(matches!(
        app.conversation.messages.last(),
        Some(thunder_agent_loop::ChatMessage::Tool { .. })
    ));

    // 4. Finish
    app.handle_agent_finished("agent_1".to_string(), true, None, None, None, None, None);
    assert_eq!(app.agent_status, AgentStatus::Idle);
    assert_eq!(app.conversation.messages.len(), 4); // System + Assistant text + Tool Call + Tool Result
}

#[tokio::test]
async fn test_app_new_session_and_shortcuts() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    app.conversation.add_user_message("Old message");
    assert_eq!(app.conversation.messages.len(), 2); // System + User

    // Ctrl+N creates new session
    app.handle_key(
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.conversation.messages.len(), 1); // New session has System message

    // Ctrl+H toggles help
}

#[tokio::test]
async fn spinner_advances_on_tick_and_wraps() {
    let mut app = App::new("gpt-4o");
    let frames = thunder_tui::app::SPINNER_FRAMES;

    assert_eq!(app.spinner(), frames[0]);
    app.tick();
    assert_eq!(app.spinner(), frames[1], "each tick moves the spinner");

    // A full cycle returns to the same frame.
    for _ in 0..frames.len() {
        app.tick();
    }
    assert_eq!(app.spinner(), frames[1]);
}

#[tokio::test]
async fn input_line_shows_a_spinner_and_working_while_running() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("gpt-4o");
    let theme = Theme::default();

    let screen = |app: &mut App| -> String {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| thunder_tui::ui::draw(f, app, &theme))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    };

    // Idle: the prompt, and no busy indicator.
    let idle = screen(&mut app);
    assert!(idle.contains('❯'), "idle prompt: {idle:?}");
    assert!(!idle.contains("working"), "not busy while idle");

    app.agent_status = AgentStatus::Streaming;
    let busy = screen(&mut app);
    assert!(busy.contains("working"), "busy hint: {busy:?}");
    assert!(
        thunder_tui::app::SPINNER_FRAMES
            .iter()
            .any(|f| busy.contains(f)),
        "spinner glyph: {busy:?}"
    );
}

#[tokio::test]
async fn input_caret_moves_to_home_and_end() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.set_input("hello world".to_string());
    assert_eq!(
        app.input_cursor, 11,
        "programmatic input parks the caret at the end"
    );

    let press = |app: &mut App, code: KeyCode, mods: KeyModifiers| {
        app.handle_key(KeyEvent::new(code, mods), tx.clone());
    };

    // Plain arrows step one character.
    press(&mut app, KeyCode::Left, KeyModifiers::empty());
    assert_eq!(app.input_cursor, 10);
    press(&mut app, KeyCode::Right, KeyModifiers::empty());
    assert_eq!(app.input_cursor, 11);

    // Cmd+Left / Cmd+Right — macOS reports Command as SUPER.
    press(&mut app, KeyCode::Left, KeyModifiers::SUPER);
    assert_eq!(app.input_cursor, 0);
    press(&mut app, KeyCode::Right, KeyModifiers::SUPER);
    assert_eq!(app.input_cursor, 11);

    // Home / End do the same while there is text to move through.
    press(&mut app, KeyCode::Home, KeyModifiers::empty());
    assert_eq!(app.input_cursor, 0);
    press(&mut app, KeyCode::End, KeyModifiers::empty());
    assert_eq!(app.input_cursor, 11);

    // Ctrl+A / Ctrl+E are the fallback for terminals that forward neither
    // Command nor Home/End.
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(app.input_cursor, 0);
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(app.input_cursor, 11);

    // Option/Alt steps by word.
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.input_cursor, 6);
    press(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.input_cursor, 0);
    press(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(app.input_cursor, 6);
}

#[tokio::test]
async fn input_edits_apply_at_the_caret() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    let press = |app: &mut App, code: KeyCode| {
        app.handle_key(KeyEvent::new(code, KeyModifiers::empty()), tx.clone());
    };

    app.set_input("hello world".to_string());
    app.cursor_home();
    app.cursor_right(); // between 'h' and 'e'

    press(&mut app, KeyCode::Char('X'));
    assert_eq!(app.input, "hXello world");
    assert_eq!(app.input_cursor, 2);

    // Backspace deletes behind the caret.
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.input, "hello world");
    assert_eq!(app.input_cursor, 1);

    // Delete removes the character under it.
    press(&mut app, KeyCode::Delete);
    assert_eq!(app.input, "hllo world");
    assert_eq!(app.input_cursor, 1);

    // Multi-byte text is stepped by character, never split mid-byte.
    app.set_input("你好世界".to_string());
    app.cursor_home();
    app.cursor_right();
    press(&mut app, KeyCode::Char('X'));
    assert_eq!(app.input, "你X好世界");
    assert_eq!(app.input_cursor, 2);
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.input, "你好世界");
    assert_eq!(app.input_cursor, 1);
}

#[tokio::test]
async fn caret_does_not_hide_the_character_under_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("gpt-4o");
    app.set_input("abc".to_string());
    app.cursor_home();

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, &mut app, &theme))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();

    // The character under the caret is highlighted, not dropped.
    assert!(screen.contains("abc"), "input renders in full: {screen:?}");
}

#[tokio::test]
async fn test_app_scrolling_and_mouse() {
    let mut app = App::new("gpt-4o");
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
