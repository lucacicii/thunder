//! The transcript pane reuses its build across frames and paints only the
//! visible slice of it.
//!
//! Both are load-bearing and easy to get subtly wrong: a stale cache paints an
//! answer that is no longer there, and a mis-measured window paints the wrong
//! rows — or drops the tail, which is what a reader is always looking at.

use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_agent_loop::types::message::ChatMessage;
use thunder_tui::prelude::*;

/// A transcript of `count` one-row lines, `LN000` … `LN199`, with markdown off
/// so every source line is exactly one screen row: the row map is then trivially
/// the line index, which makes a wrong window obvious.
fn numbered_app(count: usize) -> App {
    let mut app = App::new("gpt-4o");
    app.markdown_enabled = false;
    app.conversation.messages.clear();
    let body: String = (0..count).map(|i| format!("LN{i:03}\n")).collect();
    app.conversation.add_assistant_message(Some(body), None);
    app
}

/// Paints one frame and returns the screen as one string per row.
fn screen_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// Numbered lines on screen — i.e. transcript content rows, as opposed to the
/// pane borders, the metrics bar, the editor and the footer.
fn content_rows(rows: &[String]) -> usize {
    rows.iter().filter(|row| row.contains("LN")).count()
}

fn moved(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    }
}

#[tokio::test]
async fn the_painted_window_matches_a_full_render() {
    let mut app = numbered_app(200);
    let short = screen_rows(&mut app, 40, 24);
    let max_scroll = app.last_max_scroll;
    let visible = content_rows(&short);
    assert!(
        visible > 4,
        "a 24-row frame must show several transcript rows: {short:?}"
    );

    // A frame tall enough to hold the whole transcript, taken as the reference.
    let mut reference = numbered_app(200);
    let tall = screen_rows(&mut reference, 40, 400);
    assert_eq!(
        content_rows(&tall),
        200,
        "the reference frame holds every line"
    );

    // Content starts under the pane's top border, which sits under the header.
    for k in 0..visible {
        assert_eq!(
            short[2 + k],
            tall[2 + max_scroll + k],
            "content row {k} of the tail window differs from the full render"
        );
    }

    // The same has to hold for a window parked in the middle.
    let mut app = numbered_app(200);
    app.auto_scroll = false;
    app.scroll_offset = 37;
    let shifted = screen_rows(&mut app, 40, 24);
    let visible = content_rows(&shifted);
    for k in 0..visible {
        assert_eq!(
            shifted[2 + k],
            tall[2 + 37 + k],
            "content row {k} of the scrolled window differs from the full render"
        );
    }
}

#[tokio::test]
async fn the_tail_of_a_long_transcript_stays_reachable() {
    let mut app = numbered_app(200);
    let rows = screen_rows(&mut app, 40, 24);
    let screen = rows.join("\n");
    assert!(screen.contains("LN199"), "auto-scroll lands on the tail");
    assert!(!screen.contains("LN000"), "and not on the head");

    app.scroll_to_top();
    let rows = screen_rows(&mut app, 40, 24);
    let screen = rows.join("\n");
    assert!(screen.contains("LN000"), "the head is reachable");
    assert!(
        !screen.contains("LN199"),
        "and the tail is off screen there"
    );
}

#[tokio::test]
async fn an_appended_message_reaches_the_screen() {
    let mut app = numbered_app(5);
    screen_rows(&mut app, 40, 24);

    // Appending changes the message count, which is part of the cache key, so
    // this repaints without the caller having to invalidate anything.
    app.conversation
        .add_assistant_message(Some("BRAND-NEW-TAIL".to_string()), None);

    let rows = screen_rows(&mut app, 40, 24);
    assert!(
        rows.iter().any(|row| row.contains("BRAND-NEW-TAIL")),
        "the appended message must be on screen: {rows:?}"
    );
}

#[tokio::test]
async fn an_in_place_rewrite_reaches_the_screen_after_invalidation() {
    let mut app = numbered_app(5);
    screen_rows(&mut app, 40, 24);

    // Same character count, same message count: only the explicit invalidation
    // can tell the pane that what it cached is no longer true.
    if let Some(ChatMessage::Assistant { content, .. }) = app.conversation.messages.last_mut() {
        *content = Some(content.as_ref().unwrap().replace("LN004", "ZZ004"));
    }
    app.invalidate_transcript();

    let rows = screen_rows(&mut app, 40, 24);
    assert!(
        rows.iter().any(|row| row.contains("ZZ004")),
        "the rewritten line must be on screen: {rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("LN004")));
}

#[tokio::test]
async fn the_cache_key_follows_the_raw_toggle() {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.conversation
        .add_assistant_message(Some("## Heading\n\ntext\n".to_string()), None);

    let rendered = screen_rows(&mut app, 60, 20).join("\n");
    assert!(
        rendered.contains("Heading"),
        "markdown renders: {rendered:?}"
    );
    assert!(!rendered.contains("##"), "and consumes its markers");

    // Toggling raw mode is part of the cache key, so the cached markdown must
    // not survive it.
    app.markdown_enabled = false;
    let raw = screen_rows(&mut app, 60, 20).join("\n");
    assert!(
        raw.contains("## Heading"),
        "raw mode shows the source: {raw:?}"
    );
}

#[tokio::test]
async fn an_idle_tick_asks_for_no_frame() {
    let mut app = App::new("gpt-4o");
    app.needs_redraw = false;
    app.tick();
    assert!(
        !app.needs_redraw,
        "nothing is animating, so an idle tick owes the terminal nothing"
    );
}

#[tokio::test]
async fn a_live_tick_asks_for_a_frame() {
    let mut app = App::new("gpt-4o");
    app.agent_status = AgentStatus::Thinking;
    app.needs_redraw = false;
    app.tick();
    assert!(
        app.needs_redraw,
        "the spinner and telemetry move with the tick"
    );
}

#[tokio::test]
async fn a_key_asks_for_a_frame() {
    let mut app = App::new("gpt-4o");
    app.needs_redraw = false;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            KeyModifiers::empty(),
        ),
        tx,
    );
    assert!(app.needs_redraw, "the caret and the editor moved");
}

#[tokio::test]
async fn pointer_motion_over_dead_space_asks_for_no_frame() {
    let mut app = App::new("gpt-4o");
    app.needs_redraw = false;

    // Nothing hovered, still nothing hovered: the terminal reports every
    // pointer move, and none of them may cost a frame.
    app.handle_mouse(moved(0, 0));
    assert!(
        !app.needs_redraw,
        "motion that changes no highlight must not repaint"
    );

    // Leaving a highlighted rail row changes what is on screen.
    app.timeline_hover = Some(4);
    app.handle_mouse(moved(0, 0));
    assert!(app.needs_redraw, "leaving a rail row repaints the rail");
    assert_eq!(app.timeline_hover, None);
}
