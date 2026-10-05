//! The timeline rail: one tick per user turn, floating on the transcript's right
//! edge, and the jump it performs when a tick is clicked or entered.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_agent_loop::types::message::ToolCall;
use thunder_tui::prelude::*;
use tokio::sync::mpsc;

/// A conversation with `turns` user turns, each followed by a long answer, so
/// the transcript overflows and any turn can be jumped to the top. The first
/// turn also issues a tool call, so the rail has one to count.
fn app_with_turns(turns: usize) -> App {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    for i in 1..=turns {
        app.conversation.add_user_message(format!("TURN-{i}-MARK"));
        if i == 1 {
            app.conversation.add_assistant_message(
                None,
                Some(vec![ToolCall::new_function("call_1", "bash", "{}")]),
            );
            app.conversation
                .add_tool_message("call_1", "ok".to_string(), Some("bash".to_string()));
        }
        let answer = (0..20)
            .map(|line| format!("answer {i}.{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.conversation.add_assistant_message(Some(answer), None);
    }
    app
}

fn render_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn click(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    });
}

fn hover(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    });
}

#[test]
fn the_rail_charts_every_user_turn() {
    let mut app = app_with_turns(3);
    render_rows(&mut app, 140, 30);

    assert_eq!(app.timeline_marks.len(), 3);
    assert_eq!(app.timeline_marks[0].index, 1);
    assert_eq!(app.timeline_marks[0].prompt, "TURN-1-MARK");
    assert_eq!(app.timeline_marks[0].tools, 1);
    assert_eq!(app.timeline_marks[2].tools, 0);
    assert!(
        app.timeline_marks.windows(2).all(|w| w[1].row > w[0].row),
        "turns must be ordered down the transcript: {:?}",
        app.timeline_marks
    );
    assert_eq!(app.timeline_hitboxes.len(), 3, "every node is clickable");
}

#[test]
fn local_slash_command_echoes_are_not_turns() {
    let mut app = app_with_turns(1);
    app.conversation.add_user_message("/stats".to_string());
    app.conversation
        .add_assistant_message(Some("stats go here".to_string()), None);

    render_rows(&mut app, 140, 30);

    assert_eq!(
        app.timeline_marks.len(),
        1,
        "the command echo is bookkeeping"
    );
    assert_eq!(app.timeline_marks[0].prompt, "TURN-1-MARK");
}

#[test]
fn the_rail_stays_out_of_the_way_when_there_is_no_room_or_nothing_to_index() {
    let mut app = app_with_turns(3);
    render_rows(&mut app, 80, 30);
    assert!(
        app.timeline_hitboxes.is_empty(),
        "a narrow pane keeps the columns for the transcript"
    );
    assert_eq!(app.timeline_marks.len(), 3, "the marks are still measured");

    render_rows(&mut app, 140, 30);
    assert_eq!(app.timeline_hitboxes.len(), 3);

    app.set_timeline_visible(false);
    render_rows(&mut app, 140, 30);
    assert!(app.timeline_hitboxes.is_empty(), "the switch is absolute");

    let mut single = app_with_turns(1);
    render_rows(&mut single, 140, 30);
    assert!(
        single.timeline_hitboxes.is_empty(),
        "one turn is not a timeline"
    );
}

#[test]
fn clicking_a_node_puts_that_turn_at_the_top_of_the_transcript() {
    let mut app = app_with_turns(4);
    render_rows(&mut app, 140, 30);
    let node = *app
        .timeline_hitboxes
        .iter()
        .find(|node| node.index == 2)
        .expect("turn 2 has a node");
    assert!(
        app.auto_scroll,
        "a fresh transcript is pinned to the bottom"
    );

    click(&mut app, node.col_start + 1, node.row);

    assert!(!app.auto_scroll, "jumping is a deliberate scroll");
    assert_eq!(app.scroll_offset, app.timeline_marks[1].row);

    // The pane paints that turn's header on its first content row: the header
    // chrome takes rows 0 and 1, so content starts at row 2.
    let rows = render_rows(&mut app, 140, 30);
    assert!(rows[2].contains("You"), "header on top: {:?}", rows[2]);
    let visible = rows[2..6].join("\n");
    assert!(
        visible.contains("TURN-2-MARK"),
        "prompt in view: {visible:?}"
    );
    assert!(
        !visible.contains("TURN-1-MARK"),
        "the previous turn scrolled off: {visible:?}"
    );
}

#[test]
fn hovering_a_node_reveals_what_the_tick_cannot_show() {
    let mut app = app_with_turns(3);
    render_rows(&mut app, 140, 30);
    let node = *app
        .timeline_hitboxes
        .iter()
        .find(|node| node.index == 2)
        .expect("turn 2 has a node");

    hover(&mut app, node.col_start + 1, node.row);
    assert_eq!(app.timeline_hover, Some(2));
    assert_eq!(
        app.timeline_hover,
        app.timeline_at(node.col_start + 1, node.row)
    );

    let screen = render_rows(&mut app, 140, 30).join("\n");
    assert!(screen.contains("#2 · done · 0 tools"), "hint: {screen:?}");
    assert!(screen.contains("TURN-2-MARK"), "excerpt: {screen:?}");

    hover(&mut app, 5, 5);
    assert_eq!(app.timeline_hover, None);
    let screen = render_rows(&mut app, 140, 30).join("\n");
    assert!(
        !screen.contains("#2 · done"),
        "the hint leaves with the mouse"
    );
}

#[test]
fn the_keyboard_walks_the_rail_and_esc_hands_focus_back() {
    let mut app = app_with_turns(4);
    let (tx, _rx) = mpsc::unbounded_channel();
    render_rows(&mut app, 140, 30);

    app.cycle_focus(); // Input ➔ Chat
    app.cycle_focus(); // Chat ➔ rail
    assert_eq!(app.focus, FocusPane::Monitor);
    assert_eq!(app.timeline_selected, Some(4), "parked on the turn in view");

    app.handle_key(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.timeline_selected, Some(3));
    app.handle_key(
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.timeline_selected, Some(1));

    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        tx.clone(),
    );
    assert!(!app.auto_scroll);
    assert_eq!(app.scroll_offset, app.timeline_marks[0].row);
    assert_eq!(app.scroll_offset, 0, "the first turn starts the transcript");

    // A run in flight must survive leaving the rail.
    app.agent_status = AgentStatus::Thinking;
    app.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        tx.clone(),
    );
    assert_eq!(app.focus, FocusPane::Input);
    assert!(
        app.is_running(),
        "Esc left the rail, it did not cancel the run"
    );
}

#[test]
fn ctrl_t_toggles_the_rail_without_stranding_focus() {
    let mut app = app_with_turns(3);
    let (tx, _rx) = mpsc::unbounded_channel();
    render_rows(&mut app, 140, 30);
    app.focus = FocusPane::Monitor;

    app.handle_key(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert!(!app.timeline_visible);
    assert_eq!(
        app.focus,
        FocusPane::Input,
        "focus cannot stay on a hidden pane"
    );
    render_rows(&mut app, 140, 30);
    assert!(app.timeline_hitboxes.is_empty());

    app.handle_key(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        tx.clone(),
    );
    assert!(app.timeline_visible);
    render_rows(&mut app, 140, 30);
    assert_eq!(app.timeline_hitboxes.len(), 3);
}

#[test]
fn a_long_conversation_scrolls_the_rail_window_rather_than_clipping_it() {
    let mut app = app_with_turns(30);
    render_rows(&mut app, 140, 30);

    let shown = app.timeline_hitboxes.len();
    assert!(shown < 30, "the rail is capped, not stretched: {shown}");
    assert_eq!(
        app.timeline_hitboxes[0].index,
        30 - shown + 1,
        "pinned to the tail it must show the tail"
    );

    app.scroll_to_top();
    render_rows(&mut app, 140, 30);

    assert_eq!(app.timeline_hitboxes.len(), shown);
    assert_eq!(
        app.timeline_hitboxes[0].index, 1,
        "the window follows the viewport"
    );
    assert_eq!(app.timeline_hitboxes.last().unwrap().index, shown);
}
