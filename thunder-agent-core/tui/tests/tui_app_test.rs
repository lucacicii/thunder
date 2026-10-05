use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use thunder_agent_loop::prelude::SteerQueues;
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::message::{ChatMessage, ContentPart, ToolCall};
use thunder_agent_loop::types::tool::ToolExecutionResult;
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

    // The turn rail is the pane after the transcript while it is on screen.
    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Monitor);

    app.cycle_focus();
    assert_eq!(app.focus, FocusPane::Input);

    // Hidden, it drops out of the cycle instead of becoming a dead stop.
    app.set_timeline_visible(false);
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

    // …on a row above the prompt, never on the prompt itself.
    let input_row = busy
        .iter()
        .position(|row| row.contains('❯'))
        .expect("the prompt row");
    assert!(
        !busy[input_row].contains("working"),
        "the input row must stay clean: {:?}",
        busy[input_row]
    );
    // Found by searching rather than by offset: the metrics bar also lives
    // between here and the prompt.
    let busy_row = busy
        .iter()
        .position(|row| row.contains("working"))
        .expect("the bus row");
    assert!(
        busy_row < input_row,
        "the indicator sits above the prompt ({busy_row} < {input_row})"
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

    // Ctrl+A selects the whole prompt; Ctrl+E drops the selection and parks
    // the caret at the end, the fallback for terminals that forward neither
    // Command nor Home/End.
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(
        app.input_cursor, 11,
        "select-all parks the caret at the end"
    );
    assert_eq!(app.selection_range(), Some(0..11));
    press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    assert_eq!(app.input_cursor, 11);
    assert_eq!(app.selection_range(), None, "Ctrl+E drops the selection");

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
            image_count: 0,
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
async fn a_steer_carries_staged_images() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(Arc::clone(&queues));
    app.agent_status = AgentStatus::Streaming;
    app.pending_images
        .push(ContentPart::image("image/png", "aGVsbG8="));
    app.set_input("look at this".to_string());
    let (tx, _rx) = mpsc::unbounded_channel();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()), tx);

    assert!(app.pending_images.is_empty(), "the image was consumed");
    let queued = queues.steering.peek();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].image_count(), 1, "the image rode along");

    // Acceptance puts the same turn — attachments included — in the transcript.
    app.handle_agent_event(ObservedEvent {
        agent_id: "tui_agent".to_string(),
        event: AgentEvent::SteerAccepted {
            turn: 2,
            behavior: "steer".to_string(),
            message: "look at this".to_string(),
            image_count: 1,
        },
    });

    let last_user = app
        .conversation
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m, ChatMessage::User { .. }))
        .expect("the accepted turn");
    assert_eq!(
        last_user.image_count(),
        1,
        "the transcript shows the attachment, not just its text"
    );
}

#[tokio::test]
async fn stopping_keeps_the_partial_answer_and_reports_no_error() {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.cancel_token = Some(tokio_util::sync::CancellationToken::new());
    app.agent_status = AgentStatus::Streaming;
    app.streaming_delta = "half an answer".to_string();
    let (tx, _rx) = mpsc::unbounded_channel();

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()), tx);

    assert_eq!(
        app.agent_status,
        AgentStatus::Stopping,
        "the run is still unwinding, so a second one must not start yet"
    );
    assert_eq!(
        app.streaming_delta, "half an answer",
        "what the user already read is not discarded"
    );

    // The engine settles and reports the run as cancelled.
    app.handle_agent_finished(
        "tui_agent".to_string(),
        false,
        Some(String::new()),
        Some(vec![
            ChatMessage::user("go"),
            ChatMessage::assistant_text("half an answer"),
        ]),
        None,
        None,
        Some("Cancelled".to_string()),
    );

    assert_eq!(
        app.agent_status,
        AgentStatus::Idle,
        "ready for the next message"
    );
    assert!(
        !app.conversation.messages.iter().any(|m| matches!(
            m,
            ChatMessage::Assistant { content: Some(c), .. } if c.starts_with('❌')
        )),
        "a deliberate stop is not an error: {:?}",
        app.conversation.messages
    );
    assert!(
        app.conversation.messages.iter().any(|m| matches!(
            m,
            ChatMessage::Assistant { content: Some(c), .. } if c == "half an answer"
        )),
        "the partial answer is in the transcript"
    );
}

#[tokio::test]
async fn a_tool_result_that_arrives_after_the_stop_still_lands() {
    fn result() -> ObservedEvent {
        ObservedEvent {
            agent_id: "tui_agent".to_string(),
            event: AgentEvent::ToolExecResult {
                turn: 1,
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                result: ToolExecutionResult {
                    output: "[cancelled by user before execution — the tool did not run]"
                        .to_string(),
                    is_error: true,
                    truncated: false,
                    original_bytes: 0,
                    duration_ms: 0,
                    telemetry: None,
                },
            },
        }
    }

    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    // A run that asked for a tool and was stopped before it ran.
    app.conversation.messages.push(ChatMessage::Assistant {
        content: Some("let me look".to_string()),
        tool_calls: Some(vec![ToolCall::new_function("call_1", "read_file", "{}")]),
        refusal: None,
        name: None,
    });
    app.active_tool_calls.push(ActiveToolCall {
        id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: "{}".to_string(),
        result: None,
        is_error: false,
        duration_ms: 0,
    });
    // The run has already settled: cancel token taken, status back to idle.
    app.cancel_token = None;
    app.agent_status = AgentStatus::Idle;

    app.handle_agent_event(result());

    let tool_messages = app
        .conversation
        .messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    assert_eq!(tool_messages, 1, "the call the run never made is answered");
    assert!(
        app.active_tool_calls.is_empty(),
        "and nothing is left running"
    );
    assert_eq!(
        app.agent_status,
        AgentStatus::Idle,
        "a finished run is not resurrected by a late event"
    );

    // A duplicate delivery must not add a second copy.
    app.handle_agent_event(result());
    assert_eq!(
        app.conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::Tool { .. }))
            .count(),
        1,
        "results arriving twice are recorded once"
    );
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

#[tokio::test]
async fn test_slash_argument_completion_through_key_handling() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    for ch in "/thinking ".chars() {
        app.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty()),
            tx.clone(),
        );
    }
    assert_eq!(app.input, "/thinking ");

    // Tab writes the value in place…
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/thinking off");
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/thinking low");

    // …Shift+Tab walks back…
    app.handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/thinking off");

    // …and ↑/↓ are peers of Tab inside the argument slot.
    app.handle_key(
        KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/thinking high");
    app.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/thinking off");

    // Tab still completes a half-typed command name, then cycles its values.
    app.set_input("/thi".to_string());
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/think ");
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/think off");

    // Free-text arguments are left alone rather than being clobbered.
    app.set_input("/model gpt-4o".to_string());
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "/model gpt-4o");

    // Outside the input pane, Tab keeps switching focus, and the rail is a stop
    // on the way back to the prompt.
    app.set_input(String::new());
    app.focus = FocusPane::Chat;
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.focus, FocusPane::Monitor);
    app.handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.focus, FocusPane::Input);
}

/// Set the selection to `start..cursor`, the way a shift-movement would.
fn select(app: &mut App, start: usize, cursor: usize) {
    app.input_selection = Some(start);
    app.input_cursor = cursor;
}

#[tokio::test]
async fn typing_and_deleting_replace_the_selection() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    let press = |app: &mut App, code: KeyCode, mods: KeyModifiers| {
        app.handle_key(KeyEvent::new(code, mods), tx.clone());
    };

    app.set_input("hello world".to_string());
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(app.selection_range(), Some(0..11), "Ctrl+A selects it all");
    press(&mut app, KeyCode::Char('y'), KeyModifiers::empty());
    assert_eq!(app.input, "y", "typing replaces the selection");
    assert_eq!(app.selection_range(), None);

    app.set_input("hello world".to_string());
    select(&mut app, 0, 5);
    press(&mut app, KeyCode::Backspace, KeyModifiers::empty());
    assert_eq!(app.input, " world", "Backspace deletes the selection");
    assert_eq!(app.input_cursor, 0);

    app.set_input("hello world".to_string());
    select(&mut app, 5, 11);
    press(&mut app, KeyCode::Delete, KeyModifiers::empty());
    assert_eq!(app.input, "hello", "Delete removes it forwards too");
    assert_eq!(app.input_cursor, 5);
}

#[tokio::test]
async fn shift_arrows_extend_and_plain_arrows_collapse() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    let press = |app: &mut App, code: KeyCode, mods: KeyModifiers| {
        app.handle_key(KeyEvent::new(code, mods), tx.clone());
    };

    app.set_input("hello".to_string());
    press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(app.selection_range(), Some(4..5), "shift extends the range");
    press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(app.selection_range(), Some(3..5));
    press(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(
        app.selection_range(),
        Some(4..5),
        "shift can shrink it again"
    );

    press(&mut app, KeyCode::Left, KeyModifiers::empty());
    assert_eq!(app.selection_range(), None, "a plain arrow drops it");
    assert_eq!(app.input_cursor, 3);
}

#[tokio::test]
async fn esc_clears_the_selection_before_the_prompt() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.set_input("keep me".to_string());
    app.input_selection = Some(0);

    app.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        tx.clone(),
    );

    assert_eq!(app.selection_range(), None);
    assert_eq!(app.input, "keep me", "Esc with a selection keeps the text");
}

#[tokio::test]
async fn recalling_history_drops_the_selection() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.input_history = vec!["older".to_string()];
    app.set_input("now".to_string());
    app.input_selection = Some(0);

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()), tx);

    assert_eq!(app.input, "older");
    assert_eq!(app.selection_range(), None);
}

#[tokio::test]
async fn ctrl_c_copies_a_selection_without_touching_the_prompt() {
    use std::sync::Mutex;

    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&copied);
    app.clipboard = Some(Arc::new(move |text: &str| {
        sink.lock().unwrap().push(text.to_string());
        Ok(())
    }));

    app.set_input("hello world".to_string());
    select(&mut app, 0, 5);
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx);

    assert_eq!(*copied.lock().unwrap(), vec!["hello".to_string()]);
    assert_eq!(app.input, "hello world", "copying leaves the prompt alone");
    assert_eq!(app.selection_range(), Some(0..5), "and keeps it selected");
}

#[tokio::test]
async fn ctrl_c_clears_a_draft_before_cancelling_or_quitting() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.cancel_token = Some(tokio_util::sync::CancellationToken::new());
    app.set_input("half a thought".to_string());

    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx);

    assert!(app.input.is_empty(), "the first Ctrl+C clears the draft");
    assert!(app.cancel_token.is_some(), "it does not cancel the run yet");
    assert!(!app.should_quit);
}

#[tokio::test]
async fn ctrl_c_stops_the_run_then_quits_when_idle() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    let token = tokio_util::sync::CancellationToken::new();
    app.cancel_token = Some(token.clone());

    app.handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert!(token.is_cancelled(), "an empty prompt cancels the live run");
    assert!(!app.should_quit);

    // Once the run has unwound, Ctrl+C means quit.
    app.cancel_token = None;
    app.agent_status = AgentStatus::Idle;
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), tx);
    assert!(app.should_quit);
}

#[tokio::test]
async fn a_selection_paints_a_highlight_behind_the_text() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("gpt-4o");
    app.set_input("hello".to_string());
    app.input_selection = Some(0);

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, &mut app, &theme))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let selection_bg = ratatui::style::Color::Rgb(30, 58, 95);
    let highlighted: String = buffer
        .content()
        .iter()
        .filter(|cell| cell.bg == selection_bg)
        .map(|cell| cell.symbol())
        .collect();
    assert_eq!(
        highlighted, "hello",
        "exactly the selected text carries the selection background"
    );
}

#[tokio::test]
async fn the_help_overlay_scrolls_to_its_tail() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.handle_key(
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert_eq!(app.mode, ViewMode::Help);

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    let screen = |terminal: &Terminal<TestBackend>| -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    };

    terminal
        .draw(|f| thunder_tui::ui::draw(f, &mut app, &theme))
        .unwrap();
    assert!(
        app.help_max_scroll > 0,
        "the reference is taller than the modal, so it must scroll"
    );
    let top = screen(&terminal);
    assert!(top.contains("THUNDER TUI"), "it opens at the top");
    assert!(
        !top.contains("/quit"),
        "and the tail is genuinely below the fold: {top:?}"
    );

    // End walks to the tail; the whole reference is now reachable.
    app.handle_key(
        KeyEvent::new(KeyCode::End, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.help_scroll, app.help_max_scroll);
    terminal
        .draw(|f| thunder_tui::ui::draw(f, &mut app, &theme))
        .unwrap();
    let bottom = screen(&terminal);
    assert!(
        bottom.contains("/quit"),
        "the tail of the reference is on screen: {bottom:?}"
    );
    assert_ne!(top, bottom);
}

#[tokio::test]
async fn help_keys_do_not_reach_the_prompt_underneath() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    app.help_max_scroll = 5;
    app.mode = ViewMode::Help;
    app.set_input("draft".to_string());

    app.handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.input, "draft", "the hidden prompt takes no edits");

    let press = |app: &mut App, code: KeyCode| {
        app.handle_key(KeyEvent::new(code, KeyModifiers::empty()), tx.clone());
    };
    press(&mut app, KeyCode::Down);
    assert_eq!(app.help_scroll, 1);
    for _ in 0..20 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.help_scroll, 5, "scrolling stops at the bottom");
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.help_scroll, 0, "g returns to the top");
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.help_scroll, 5, "a page clamps too");
    press(&mut app, KeyCode::Home);
    assert_eq!(app.help_scroll, 0);
}
