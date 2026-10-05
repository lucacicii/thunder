//! The inline rename mode: what the prompt box says while it is collecting a
//! session name, and that the command popup stays out of the way.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_tui::prelude::*;
use tokio::sync::mpsc;

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
fn a_bare_rename_prompts_in_the_input_box() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/rename", tx));

    let renamed = screen(&mut app, 80, 20);
    assert!(renamed.contains("Enter a new session name..."), "{renamed}");
    assert!(renamed.contains("✎"), "the prompt glyph marks the mode");
    assert!(
        renamed.contains("Type a new session name"),
        "hinted in the footer"
    );
}

#[test]
fn a_name_that_starts_with_a_slash_is_not_a_command() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/rename", tx));

    // Typing the name re-enables nothing: the popup would cover the hint and
    // Tab would rewrite the name into a command.
    app.set_input("/resume".to_string());
    let typing = screen(&mut app, 80, 20);
    assert!(!typing.contains("Slash Commands"), "{typing}");

    app.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ),
        mpsc::unbounded_channel().0,
    );
    assert_eq!(app.input, "/resume", "Tab must not complete a command here");
}

#[test]
fn ctrl_c_backs_out_of_a_rename_instead_of_quitting() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/rename", tx));

    app.handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        mpsc::unbounded_channel().0,
    );
    assert!(!app.rename_mode);
    assert!(
        !app.should_quit,
        "an empty rename box cancels, it never quits"
    );
}
