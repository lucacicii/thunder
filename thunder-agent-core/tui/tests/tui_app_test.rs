use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use thunder_agent_loop::prelude::SteerQueues;
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
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
async fn busy_indicator_sits_above_the_input_not_in_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("gpt-4o");
    let theme = Theme::default();

    // Draws a frame and returns the screen rows.
    let rows = |app: &mut App| -> Vec<String> {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| thunder_tui::ui::draw(f, app, &theme))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area().width as usize;
        buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    };

    let idle = rows(&mut app);
    let idle_screen = idle.join("\n");
    assert!(idle_screen.contains('❯'), "idle prompt: {idle_screen:?}");
    assert!(!idle_screen.contains("working"), "idle shows no busy hint");

    app.agent_status = AgentStatus::Streaming;
    let busy = rows(&mut app);
    let busy_screen = busy.join("\n");

    // The spinner and its label are drawn…
    assert!(
        busy_screen.contains("working"),
        "busy hint: {busy_screen:?}"
    );
    assert!(
        thunder_tui::app::SPINNER_FRAMES
            .iter()
            .any(|f| busy_screen.contains(f)),
        "spinner glyph: {busy_screen:?}"
    );

    // …on the separator above the prompt, never on the prompt itself.
    let input_row = busy
        .iter()
        .position(|row| row.contains('❯'))
        .expect("the prompt row");
    assert!(
        !busy[input_row].contains("working"),
        "the input row must stay clean: {:?}",
        busy[input_row]
    );
    assert!(
        busy[input_row - 1].contains("working"),
        "the separator above the input carries it: {:?}",
        busy[input_row - 1]
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
async fn enter_while_running_steers_instead_of_submitting() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    app.agent_status = AgentStatus::Streaming;
    app.set_input("change of plan".to_string());
    let (tx, _rx) = mpsc::unbounded_channel();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx);

    assert_eq!(queues.steering.len(), 1, "Enter steers the live run");
    assert!(queues.follow_up.is_empty());
    assert!(app.input.is_empty(), "the editor is cleared after queueing");
    assert_eq!(app.queued.steering, vec!["change of plan".to_string()]);
    assert!(
        !app.conversation
            .messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User { .. })),
        "nothing was submitted as a new turn"
    );
}

#[tokio::test]
async fn alt_enter_while_running_queues_a_follow_up() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    app.agent_status = AgentStatus::Thinking;
    app.set_input("then summarise".to_string());
    let (tx, _rx) = mpsc::unbounded_channel();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), tx);

    assert_eq!(queues.follow_up.len(), 1, "Alt+Enter queues a follow-up");
    assert!(queues.steering.is_empty());
    assert_eq!(app.queued.follow_up, vec!["then summarise".to_string()]);
}

#[tokio::test]
async fn a_queued_message_enters_the_transcript_when_accepted() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    app.agent_status = AgentStatus::Streaming;
    queues.steering.enqueue(ChatMessage::user("new direction"));
    app.queued = queues.snapshot();

    app.handle_agent_event(ObservedEvent {
        agent_id: "tui_agent".to_string(),
        event: AgentEvent::SteerAccepted {
            turn: 2,
            behavior: "steer".to_string(),
            message: "new direction".to_string(),
        },
    });

    assert!(
        app.conversation.messages.iter().any(|m| matches!(
            m,
            ChatMessage::User { content, .. } if content == "new direction"
        )),
        "the accepted message is shown immediately"
    );
}

#[tokio::test]
async fn cancelling_returns_queued_text_to_the_editor() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    app.agent_status = AgentStatus::Streaming;
    app.cancel_token = Some(tokio_util::sync::CancellationToken::new());
    let (tx, _rx) = mpsc::unbounded_channel();

    app.set_input("do this instead".to_string());
    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        tx.clone(),
    );
    app.set_input("then summarise".to_string());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), tx.clone());
    assert_eq!(queues.steering.len() + queues.follow_up.len(), 2);

    // Esc cancels the run — and hands the queued text back rather than dropping it.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()), tx);

    assert!(app.input.contains("do this instead"), "got {:?}", app.input);
    assert!(app.input.contains("then summarise"), "got {:?}", app.input);
    assert_eq!(app.queued.total(), 0, "the pending list is emptied");
    assert!(queues.steering.is_empty() && queues.follow_up.is_empty());
}

#[tokio::test]
async fn queue_command_reports_and_clears() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    queues.steering.enqueue(ChatMessage::user("queued text"));
    let (tx, _rx) = mpsc::unbounded_channel();

    app.execute_slash_command("/queue", tx.clone());
    let listed = app
        .conversation
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            ChatMessage::Assistant { content, .. } => content.clone(),
            _ => None,
        })
        .expect("a report");
    assert!(listed.contains("queued text"), "got {listed:?}");

    app.execute_slash_command("/queue clear", tx);
    assert!(app.input.contains("queued text"), "got {:?}", app.input);
    assert_eq!(app.queued.total(), 0);
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
