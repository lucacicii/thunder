//! The frame's static chrome: the header above the transcript, and the promise
//! that a fresh frame is written entirely in English.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_tui::prelude::*;

/// Renders a frame and returns every cell's symbol, concatenated.
fn render(app: &mut App, width: u16, height: u16) -> String {
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

/// The first screen row, which is the header.
fn header_row(app: &mut App, width: u16) -> String {
    let row_width = width as usize;
    render(app, width, 24).chars().take(row_width).collect()
}

fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        // CJK symbols/punctuation, kana, CJK ext-A, CJK, hangul, fullwidth forms
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF | 0xFF00..=0xFFEF
    )
}

#[test]
fn the_header_row_names_the_model_and_its_thinking_level() {
    let mut app = App::new("gpt-4o");
    app.thinking_level = Some("high".to_string());

    let header = header_row(&mut app, 140);

    // The buffer pads a double-width emoji with a blank cell, so match on the
    // word rather than the glyph-plus-space.
    assert!(header.contains("THUNDER"), "brand: {header:?}");
    assert!(header.contains("openai/gpt-4o"), "model: {header:?}");
    assert!(header.contains("think:high"), "thinking level: {header:?}");
    assert!(header.contains("Session:"), "session title: {header:?}");
    assert!(
        !header.contains("Tokens: ~"),
        "the metrics bar owns the counts: {header:?}"
    );
    assert!(header.contains("IDLE"), "status badge: {header:?}");
}

#[test]
fn a_fresh_frame_is_written_in_english() {
    let mut app = App::new("gpt-4o");
    app.thinking_level = Some("high".to_string());

    let screen = render(&mut app, 160, 30);
    let cjk: String = screen.chars().filter(|c| is_cjk(*c)).collect();

    assert!(
        cjk.is_empty(),
        "no Chinese anywhere in a fresh frame, found {cjk:?}"
    );
    // The system prompt is part of the transcript, so it is part of this too.
    assert!(screen.contains("Role & Philosophy"), "prompt is rendered");
}
