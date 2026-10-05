//! The row under the input box always names the session, and a transient status
//! message stacks above it instead of replacing it.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_tui::prelude::*;

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

fn titled(title: &str) -> App {
    let mut app = App::new("gpt-4o");
    app.conversation.title = Some(title.to_string());
    app
}

#[test]
fn the_last_row_names_the_session() {
    let mut app = titled("refactor the prompt box");
    let rows = render_rows(&mut app, 100, 20);

    assert!(
        rows.last().unwrap().starts_with(" refactor the prompt box"),
        "footer is left aligned: {:?}",
        rows.last()
    );
    assert!(
        !rows[0].contains("refactor the prompt box"),
        "the header no longer carries the session name: {:?}",
        rows[0]
    );
}

#[test]
fn an_untitled_session_falls_back_to_its_id() {
    let mut app = App::new("gpt-4o");
    let id = app.conversation.id.clone();
    let rows = render_rows(&mut app, 100, 20);
    let last = rows.last().unwrap();
    assert!(last.contains(&id), "id in the footer: {last:?}");
    assert!(
        !last.contains("New Conversation"),
        "not the placeholder: {last:?}"
    );
}

#[test]
fn a_long_title_is_cut_with_an_ellipsis() {
    let mut app = titled(&"wide ".repeat(40));
    let rows = render_rows(&mut app, 40, 20);
    let last = rows.last().unwrap().trim_end();

    assert!(last.ends_with('…'), "truncated: {last:?}");
    assert!(
        unicode_width::UnicodeWidthStr::width(last) <= 39,
        "must fit the pane: {last:?}",
    );
}

#[test]
fn a_status_message_stacks_above_the_session_line() {
    let mut app = titled("refactor the prompt box");
    app.set_status_message("✔ Answer sent to the agent.");
    let rows = render_rows(&mut app, 100, 20);

    assert!(
        rows[rows.len() - 2].contains("Answer sent"),
        "status sits above: {:?}",
        rows[rows.len() - 2]
    );
    assert!(
        rows.last().unwrap().contains("refactor the prompt box"),
        "the name never blinks away: {:?}",
        rows.last()
    );
}

#[test]
fn the_footer_survives_a_narrow_frame() {
    let mut app = titled("refactor the prompt box");
    app.set_status_message("✔ done");
    let rows = render_rows(&mut app, 24, 12);

    assert!(rows.last().unwrap().trim().len() <= 24);
    assert!(rows[rows.len() - 2].contains("done"));
}
