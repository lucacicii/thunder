//! The transcript's scroll bounds must match what `Paragraph` actually paints.
//!
//! Raw lines (tool output, system notices, tool-call args) are pushed verbatim,
//! so they can be wider than the pane and get word-wrapped into several screen
//! rows. Counting each as one row made `max_scroll` too small, which pinned
//! auto-scroll above the true bottom: the answer that had just arrived was off
//! screen and `Down` was a no-op (auto-scroll was already "at the bottom").

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_tui::prelude::*;

const MARKER: &str = "END-OF-ANSWER-MARKER";

/// An app with a long, wrapping tool transcript and a short final answer. The
/// answer ends in `MARKER` so a test can tell whether the tail is visible.
fn app_with_wrapping_transcript() -> App {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.conversation
        .add_user_message("run the thing".to_string());

    // Long lines that `Paragraph` wraps many times over, as real tool output
    // (diffs, logs, directory listings) routinely does.
    let tool_output: String = (0..40)
        .map(|i| format!("line {i}: {}", "x".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    app.conversation
        .add_tool_message("call_1", tool_output, Some("bash".to_string()));

    app.conversation
        .add_assistant_message(Some(format!("All done.\n\n{MARKER}")), None);
    app
}

fn draw_frame(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[tokio::test]
async fn auto_scroll_shows_the_tail_when_raw_lines_wrap() {
    let mut app = app_with_wrapping_transcript();
    assert!(app.auto_scroll);

    let screen = draw_frame(&mut app, 80, 24);

    assert!(
        app.last_max_scroll > 0,
        "the wrapped transcript must overflow the pane"
    );
    assert!(
        screen.contains(MARKER),
        "auto-scroll must land on the real bottom: {screen:?}"
    );
}

#[tokio::test]
async fn raw_mode_scroll_bounds_are_exact_too() {
    let mut app = app_with_wrapping_transcript();
    app.markdown_enabled = false;

    let screen = draw_frame(&mut app, 80, 24);

    assert!(
        screen.contains(MARKER),
        "the raw rendering path must land on the real bottom: {screen:?}"
    );
}

#[tokio::test]
async fn scrolling_down_reaches_the_tail_from_a_scrolled_up_view() {
    let mut app = app_with_wrapping_transcript();
    draw_frame(&mut app, 80, 24);

    // The user scrolls up to read, which leaves auto-scroll.
    app.scroll_up(5);
    assert!(!app.auto_scroll);

    // Scrolling back down must be able to reach the bottom the renderer reports.
    app.scroll_down(10_000);
    assert!(app.auto_scroll, "reaching the bottom re-arms auto-scroll");

    let screen = draw_frame(&mut app, 80, 24);
    assert!(
        screen.contains(MARKER),
        "the tail is reachable by scrolling down: {screen:?}"
    );
}
