//! Markdown preview and clickable links, exercised through the real draw path.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::tempdir;
use thunder_agent_loop::types::message::ChatMessage;
use thunder_tui::links::LinkTarget;
use thunder_tui::picker::PickerKind;
use thunder_tui::prelude::*;
use tokio::sync::mpsc;

/// An app whose transcript holds only what the test puts in it: the default
/// system prompt and welcome message would otherwise contribute links of their
/// own and make the assertions ambiguous.
fn bare_app(workspace: &std::path::Path) -> App {
    let mut app = App::new("gpt-4o");
    app.workspace_dir = workspace.to_path_buf();
    app.conversation.messages.clear();
    app
}

/// Draws one frame, which is what populates `link_hitboxes`.
fn draw_frame(app: &mut App, width: u16, height: u16) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
}

#[tokio::test]
async fn preview_command_toggles_rendering() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    // Rendering is the default: the TUI output is a preview out of the box.
    assert!(app.markdown_enabled);

    app.execute_slash_command("/preview", tx.clone());
    assert!(!app.markdown_enabled, "bare /preview toggles");

    app.execute_slash_command("/preview on", tx.clone());
    assert!(app.markdown_enabled);

    app.execute_slash_command("/preview off", tx.clone());
    assert!(!app.markdown_enabled);

    // The alias works too.
    app.execute_slash_command("/md on", tx);
    assert!(app.markdown_enabled);
}

#[tokio::test]
async fn links_command_collects_paths_and_urls() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("report.md"), "x").unwrap();

    let mut app = bare_app(dir.path());
    app.conversation.add_assistant_message(
        Some("Wrote `report.md` — see [docs](https://example.com)".to_string()),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    app.execute_slash_command("/links", tx);

    assert!(app.picker.is_open);
    assert_eq!(app.picker.kind, PickerKind::SelectLink);
    // The id is an index into `pending_links`.
    assert_eq!(app.pending_links.len(), 2, "{:?}", app.pending_links);
    assert_eq!(app.pending_links.len(), app.picker.items.len());
    assert!(app
        .pending_links
        .contains(&LinkTarget::File(dir.path().join("report.md"))));
    assert!(app
        .pending_links
        .contains(&LinkTarget::Url("https://example.com".into())));

    let badges: Vec<&str> = app
        .picker
        .items
        .iter()
        .filter_map(|i| i.badge.as_deref())
        .collect();
    assert!(
        badges.contains(&"file") && badges.contains(&"web"),
        "{badges:?}"
    );
}

#[tokio::test]
async fn links_command_ignores_the_system_prompt() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("secret.md"), "x").unwrap();

    let mut app = bare_app(dir.path());
    app.conversation.messages.push(ChatMessage::System {
        content: "internal note about `secret.md`".to_string(),
        name: None,
    });

    let (tx, _rx) = mpsc::unbounded_channel();
    app.execute_slash_command("/links", tx);
    assert!(
        !app.picker.is_open,
        "a path only the system prompt mentions must not be offered"
    );
}

#[tokio::test]
async fn links_command_reports_when_there_is_nothing_to_open() {
    let dir = tempdir().unwrap();
    let mut app = bare_app(dir.path());
    app.conversation
        .add_assistant_message(Some("No targets in here at all.".to_string()), None);

    let (tx, _rx) = mpsc::unbounded_channel();
    app.execute_slash_command("/links", tx);
    assert!(!app.picker.is_open, "an empty session opens no picker");
}

#[tokio::test]
async fn rendered_paths_are_clickable_at_their_screen_cell() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("report.md"), "x").unwrap();

    let mut app = bare_app(dir.path());
    app.conversation
        .add_assistant_message(Some("Wrote `report.md` just now".to_string()), None);

    draw_frame(&mut app, 100, 30);

    let hitbox = app
        .link_hitboxes
        .iter()
        .find(|h| h.target == LinkTarget::File(dir.path().join("report.md")))
        .expect("the path should be a link")
        .clone();

    // A cell inside the hitbox resolves to the same target…
    assert_eq!(
        app.link_at(hitbox.col_start, hitbox.row),
        Some(hitbox.target.clone())
    );
    assert_eq!(
        app.link_at(hitbox.col_end - 1, hitbox.row),
        Some(hitbox.target.clone())
    );
    // …and a cell past it does not.
    assert_eq!(app.link_at(hitbox.col_end + 3, hitbox.row), None);
    assert_eq!(app.link_at(hitbox.col_start, hitbox.row + 7), None);
}

#[tokio::test]
async fn raw_mode_has_no_clickable_regions() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("report.md"), "x").unwrap();

    let mut app = bare_app(dir.path());
    app.markdown_enabled = false;
    app.conversation
        .add_assistant_message(Some("Wrote `report.md`".to_string()), None);

    draw_frame(&mut app, 100, 30);
    assert!(
        app.link_hitboxes.is_empty(),
        "raw source is not interactive"
    );
}

#[tokio::test]
async fn markdown_source_is_not_shown_verbatim() {
    let dir = tempdir().unwrap();
    let mut app = bare_app(dir.path());
    app.conversation
        .add_assistant_message(Some("## Heading\n\n- item one\n".to_string()), None);

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, &mut app, &theme))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let screen: String = buffer.content().iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("Heading"), "content renders: {screen:?}");
    assert!(
        !screen.contains("##"),
        "markdown markers must be consumed: {screen:?}"
    );
    assert!(
        !screen.contains("- item"),
        "list bullet is rendered: {screen:?}"
    );
}
