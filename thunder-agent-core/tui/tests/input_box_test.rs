//! The input box: it grows with the prompt, keeps a blank row above and below
//! the text, takes newlines from Shift+Enter and bracketed paste, and no longer
//! spends a row on static command tips.

use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use thunder_agent_loop::prelude::SteerQueues;
use thunder_tui::event::AppEvent;
use thunder_tui::prelude::*;

fn theme_bg() -> Color {
    Theme::default().bg
}

/// Draws a frame and returns the rows as (text, background of the first cell).
fn draw_rows(app: &mut App, width: u16, height: u16) -> Vec<(String, Color)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            let text = (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            (text, buffer[(0, y)].style().bg.unwrap_or(Color::Reset))
        })
        .collect()
}

/// The rows the input panel paints: its background spans the padding too.
fn box_rows(rows: &[(String, Color)]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, (_, bg))| *bg == theme_bg())
        .map(|(y, _)| y)
        .collect()
}

fn prompt_row(rows: &[(String, Color)]) -> usize {
    rows.iter()
        .position(|(text, _)| text.contains('❯'))
        .expect("prompt row")
}

fn screen(rows: &[(String, Color)]) -> String {
    rows.iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_input_box_grows_with_the_prompt() {
    let mut app = App::new("gpt-4o");

    let empty = draw_rows(&mut app, 60, 20);
    let empty_box = box_rows(&empty);
    assert_eq!(
        empty_box.len(),
        3,
        "one text row plus one row of padding each side: {empty_box:?}"
    );

    // Three explicit lines: three text rows plus the padding.
    app.set_input("first line\nsecond line\nthird line".to_string());
    let grown = draw_rows(&mut app, 60, 20);
    assert_eq!(
        box_rows(&grown).len(),
        5,
        "the box grows with the prompt: {}",
        screen(&grown)
    );
    let text = screen(&grown);
    for line in ["first line", "second line", "third line"] {
        assert!(text.contains(line), "{line:?} is visible: {text:?}");
    }
}

#[test]
fn soft_wrapped_lines_grow_the_box_too() {
    let mut app = App::new("gpt-4o");
    // ~120 characters in a 40-column terminal: several rows without a newline.
    app.set_input("word ".repeat(24));
    let rows = draw_rows(&mut app, 40, 20);
    assert!(
        box_rows(&rows).len() > 3,
        "a wrapped prompt makes the box taller: {:?}",
        box_rows(&rows)
    );
}

#[test]
fn the_input_box_pads_above_and_below_the_text() {
    let mut app = App::new("gpt-4o");
    let rows = draw_rows(&mut app, 60, 20);
    let prompt = prompt_row(&rows);

    // The prompt is the middle row of its box: blank panel rows on both sides.
    assert_eq!(
        rows[prompt - 1].0.trim(),
        "",
        "blank row above: {:?}",
        rows[prompt - 1].0
    );
    assert_eq!(
        rows[prompt + 1].0.trim(),
        "",
        "blank row below: {:?}",
        rows[prompt + 1].0
    );
    for y in [prompt - 1, prompt, prompt + 1] {
        assert_eq!(rows[y].1, theme_bg(), "row {y} belongs to the panel");
    }
}

#[tokio::test]
async fn shift_enter_starts_a_new_line() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_input("hi".to_string());

    app.handle_key(
        ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::SHIFT,
        ),
        tx.clone(),
    );
    app.handle_key(
        ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('x'),
            ratatui::crossterm::event::KeyModifiers::empty(),
        ),
        tx.clone(),
    );

    assert_eq!(app.input, "hi\nx", "Shift+Enter inserts a newline");
    assert_eq!(app.input_cursor, 4);
    assert!(
        app.conversation.messages.iter().all(|m| !matches!(
            m,
            thunder_agent_loop::types::message::ChatMessage::User { .. }
        )),
        "nothing was submitted"
    );

    // A plain Enter still submits.
    app.handle_key(
        ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::empty(),
        ),
        tx,
    );
    assert!(app.input.is_empty(), "Enter submits");
}

#[test]
fn shift_enter_does_not_steer_a_running_task() {
    let mut app = App::new("gpt-4o");
    let queues = SteerQueues::new_shared();
    app.steer_queues = Some(std::sync::Arc::clone(&queues));
    app.agent_status = AgentStatus::Streaming;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_input("half a thought".to_string());

    app.handle_key(
        ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::SHIFT,
        ),
        tx,
    );

    assert_eq!(app.input, "half a thought\n");
    assert!(queues.steering.is_empty(), "a newline is not a steer");
}

#[test]
fn pasting_multiple_lines_fills_the_input_without_submitting() {
    let mut app = App::new("gpt-4o");

    app.handle_paste("fn main() {\r\n    body();\r\n}".to_string());

    assert_eq!(app.input, "fn main() {\n    body();\n}");
    assert!(
        app.conversation.messages.iter().all(|m| !matches!(
            m,
            thunder_agent_loop::types::message::ChatMessage::User { .. }
        )),
        "paste never submits"
    );

    let rows = draw_rows(&mut app, 60, 20);
    let text = screen(&rows);
    assert!(text.contains("fn main() {"), "{text:?}");
    assert!(text.contains("body();"), "{text:?}");

    // The terminal listener forwards a paste as this event, as one block.
    match AppEvent::Paste("a\nb".to_string()) {
        AppEvent::Paste(text) => assert_eq!(text, "a\nb"),
        _ => unreachable!("paste arrives as its own event"),
    }
}

#[test]
fn paste_is_ignored_while_a_modal_owns_the_screen() {
    let mut app = App::new("gpt-4o");
    app.picker.is_open = true;
    app.handle_paste("ignored".to_string());
    assert!(app.input.is_empty(), "the picker owns the keyboard");
}

#[test]
fn the_static_tips_row_is_gone_and_the_row_is_reclaimed() {
    let mut app = App::new("gpt-4o");
    let rows = draw_rows(&mut app, 100, 20);
    let text = screen(&rows);

    assert!(!text.contains("/resume"), "no command tips: {text:?}");
    assert!(
        !text.contains("Tokens: ~"),
        "no duplicate token readout: {text:?}"
    );

    // Nothing to say: the last row is still part of the input panel.
    let last = rows.len() - 1;
    assert_eq!(rows[last].1, theme_bg(), "the footer row collapsed away");

    // A status message still gets its row.
    app.set_status_message("hello there");
    let rows = draw_rows(&mut app, 100, 20);
    assert!(
        screen(&rows).contains("hello there"),
        "the status line returns when it has something to say"
    );
    assert_eq!(rows[rows.len() - 1].1, Color::Rgb(10, 14, 20));
}
