//! Clicking a link must work wherever the pane painted it.
//!
//! Verbatim lines (tool output, system notices, tool-call args) are word-wrapped
//! by `Paragraph` into several screen rows, so a link's line index is not its
//! screen row. Using the index put the hitbox on a row the link was never
//! painted on — or filtered it out of the viewport entirely.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use thunder_tui::links::LinkTarget;
use thunder_tui::prelude::*;

const FILE_LINK: &str = "report.md";
const URL_LINK: &str = "https://example.com/thunder";

/// A transcript whose link sits under many wrapped rows of tool output.
fn app_with_wrapped_tool_output(answer: &str) -> App {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.conversation
        .add_user_message("do the thing".to_string());

    let logs: String = (0..20)
        .map(|i| format!("log {i}: {}", "L".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    app.conversation
        .add_tool_message("call_1", logs, Some("bash".to_string()));

    app.conversation
        .add_assistant_message(Some(answer.to_string()), None);
    app
}

/// Draws a frame and returns the screen as one string per terminal row.
fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

/// The terminal row and column a piece of text was painted at.
fn painted_at(rows: &[String], needle: &str) -> (usize, usize) {
    rows.iter()
        .enumerate()
        .find_map(|(y, row)| row.find(needle).map(|column| (y, column)))
        .unwrap_or_else(|| panic!("{needle:?} should be painted somewhere: {rows:?}"))
}

fn click(app: &mut App, column: usize, row: usize) {
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: column as u16,
        row: row as u16,
        modifiers: crossterm::event::KeyModifiers::empty(),
    });
}

fn recording_opener() -> (Arc<Mutex<Vec<LinkTarget>>>, App) {
    let mut app = app_with_wrapped_tool_output("placeholder");
    let opened: Arc<Mutex<Vec<LinkTarget>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&opened);
    app.link_opener = Some(Arc::new(move |target: &LinkTarget| {
        sink.lock().unwrap().push(target.clone());
        Ok(())
    }));
    (opened, app)
}

#[test]
fn a_file_link_after_wrapped_tool_output_is_clickable() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(FILE_LINK), "x").unwrap();

    let (opened, mut app) = recording_opener();
    app.conversation
        .add_assistant_message(Some(format!("Wrote `{FILE_LINK}` just now")), None);
    app.workspace_dir = dir.path().to_path_buf();

    let rows = screen(&mut app, 80, 24);
    let (row, column) = painted_at(&rows, FILE_LINK);
    let target = LinkTarget::File(dir.path().join(FILE_LINK));

    assert_eq!(
        app.link_at(column as u16, row as u16),
        Some(target.clone()),
        "the hitbox covers the cell the link is painted on (row {row}, column {column})"
    );

    click(&mut app, column, row);
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        &[target],
        "a click on the painted link activates it"
    );
}

#[test]
fn a_url_after_wrapped_tool_output_is_clickable() {
    let (opened, mut app) = recording_opener();
    app.conversation
        .add_assistant_message(Some(format!("See {URL_LINK}")), None);

    let rows = screen(&mut app, 80, 24);
    let (row, column) = painted_at(&rows, URL_LINK);
    let target = LinkTarget::Url(URL_LINK.to_string());

    assert_eq!(app.link_at(column as u16, row as u16), Some(target.clone()));
    click(&mut app, column, row);
    assert_eq!(opened.lock().unwrap().as_slice(), &[target]);
}

#[test]
fn link_hitboxes_follow_the_scroll_offset() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(FILE_LINK), "x").unwrap();

    let mut app = app_with_wrapped_tool_output(&format!("Wrote `{FILE_LINK}` just now"));
    app.workspace_dir = dir.path().to_path_buf();

    // Scrolled to the top the link is far below the fold, so it has no hitbox.
    app.scroll_to_top();
    screen(&mut app, 80, 24);
    assert!(app.link_hitboxes.is_empty(), "{:?}", app.link_hitboxes);

    // Back at the bottom the hitbox follows the row it is painted on.
    app.scroll_to_bottom();
    let rows = screen(&mut app, 80, 24);
    let (row, _) = painted_at(&rows, FILE_LINK);
    assert!(
        app.link_hitboxes.iter().any(|h| h.row as usize == row),
        "a hitbox lands on the painted row {row}: {:?}",
        app.link_hitboxes
    );
}
