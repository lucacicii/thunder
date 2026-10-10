//! Coverage for the `/prompt` command: reusable templates under
//! `.thunder/prompts/`, rendered with `$ARGUMENTS`.

use thunder_tui::app::App;
use tokio::sync::mpsc;

fn app_in(dir: &std::path::Path) -> App {
    let mut app = App::new("gpt-4o");
    app.workspace_dir = dir.to_path_buf();
    app
}

fn last_assistant(app: &App) -> String {
    app.conversation
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            thunder_agent_loop::types::message::ChatMessage::Assistant { content, .. } => {
                content.clone()
            }
            _ => None,
        })
        .unwrap_or_default()
}

fn write_template(dir: &std::path::Path, name: &str, body: &str) {
    let prompts = dir.join(".thunder").join("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(prompts.join(name), body).unwrap();
}

#[test]
fn prompt_list_reads_the_templates() {
    let dir = tempfile::tempdir().unwrap();
    write_template(
        dir.path(),
        "review.md",
        "---\nname: review\ndescription: Review a diff\n---\nReview $ARGUMENTS carefully.",
    );

    let mut app = app_in(dir.path());
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/prompt list", tx));

    let out = last_assistant(&app);
    assert!(out.contains("review"), "template not listed:\n{out}");
}

#[tokio::test]
async fn prompt_run_submits_the_rendered_template() {
    let dir = tempfile::tempdir().unwrap();
    write_template(
        dir.path(),
        "explain.md",
        "---\nname: explain\ndescription: Explain code\n---\nExplain the following in depth: $ARGUMENTS",
    );

    let mut app = app_in(dir.path());
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/prompt run explain src/main.rs", tx));

    // `run` submits a user turn carrying the rendered body.
    let user = app
        .conversation
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            thunder_agent_loop::types::message::ChatMessage::User { content, .. } => {
                Some(content.clone())
            }
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        user.contains("Explain the following in depth: src/main.rs"),
        "template not rendered with args:\n{user}"
    );
    assert!(
        !user.contains("$ARGUMENTS"),
        "placeholder left unsubstituted"
    );
}

#[test]
fn prompt_missing_template_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_in(dir.path());
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/prompt run nope", tx));

    let out = last_assistant(&app);
    assert!(out.contains("No prompt template"), "missing report:\n{out}");
}

#[test]
fn prompt_show_prints_the_body() {
    let dir = tempfile::tempdir().unwrap();
    write_template(
        dir.path(),
        "note.md",
        "---\nname: note\ndescription: A note template\n---\nBODY_MARKER_XYZ",
    );
    let mut app = app_in(dir.path());
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(app.execute_slash_command("/prompt show note", tx));

    let out = last_assistant(&app);
    assert!(out.contains("BODY_MARKER_XYZ"), "body not shown:\n{out}");
}
