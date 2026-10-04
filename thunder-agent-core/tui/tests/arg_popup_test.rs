//! The argument popup: that completing a command switches the popup from the
//! command list to that command's values, and that typing filters them.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_tui::prelude::*;

fn screen(app: &mut App, width: u16, height: u16) -> String {
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
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn completing_a_command_switches_the_popup_to_its_values() {
    let mut app = App::new("gpt-4o");

    app.set_input("/think".to_string());
    let commands = screen(&mut app, 60, 20);
    assert!(commands.contains("Slash Commands"), "{commands}");

    app.set_input("/thinking ".to_string());
    let values = screen(&mut app, 60, 20);
    assert!(values.contains("first argument"), "{values}");
    assert!(values.contains("off"), "{values}");
    assert!(values.contains("medium"), "{values}");
    assert!(values.contains("high"), "{values}");
}

#[test]
fn typing_a_prefix_narrows_the_values() {
    let mut app = App::new("gpt-4o");
    app.set_input("/think l".to_string());
    let filtered = screen(&mut app, 60, 20);
    assert!(filtered.contains("low"), "{filtered}");
    assert!(!filtered.contains("medium"), "{filtered}");

    // A free-text argument keeps the plain command list, not a value popup.
    app.set_input("/model gpt-4o".to_string());
    let free_text = screen(&mut app, 60, 20);
    assert!(!free_text.contains("first argument"), "{free_text}");
}
