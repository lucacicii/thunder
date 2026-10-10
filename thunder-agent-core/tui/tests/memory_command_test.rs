//! Coverage for the `/memory` slash command.
//!
//! The command is read-only and resolves the same files the memory plugin
//! injects; these tests pin the listing and the `show` lookup against a real
//! `.thunder/` tree.

use thunder_tui::app::App;
use tokio::sync::mpsc;

fn app_in(dir: &std::path::Path) -> App {
    let mut app = App::new("gpt-4o");
    app.workspace_dir = dir.to_path_buf();
    app
}

/// The last assistant message, which is where a slash command's output lands.
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

#[tokio::test]
async fn memory_list_names_the_project_files() {
    let dir = tempfile::tempdir().unwrap();
    let thunder = dir.path().join(".thunder");
    std::fs::create_dir_all(thunder.join("memory")).unwrap();
    std::fs::write(thunder.join("THUNDER.md"), "hello").unwrap();
    std::fs::write(thunder.join("memory").join("build.md"), "build notes").unwrap();

    let mut app = app_in(dir.path());
    let (tx, mut rx) = mpsc::unbounded_channel();

    assert!(app.execute_slash_command("/memory list", tx));

    // The command spawns its work and delivers the result as an event.
    let event = rx.recv().await.expect("an event should arrive");
    let content = match event {
        thunder_tui::event::AppEvent::AgentFinished { final_text, .. } => {
            final_text.unwrap_or_default()
        }
        other => panic!("unexpected event: {other:?}"),
    };

    assert!(
        content.contains("THUNDER.md"),
        "listing missing the main file:\n{content}"
    );
    assert!(
        content.contains("memory/build.md"),
        "listing missing the topic file:\n{content}"
    );
}

#[tokio::test]
async fn memory_show_reads_a_named_file() {
    let dir = tempfile::tempdir().unwrap();
    let thunder = dir.path().join(".thunder");
    std::fs::create_dir_all(&thunder).unwrap();
    std::fs::write(thunder.join("THUNDER.md"), "PROJECT_MEMORY_BODY").unwrap();

    let mut app = app_in(dir.path());
    let (tx, mut rx) = mpsc::unbounded_channel();

    assert!(app.execute_slash_command("/memory show THUNDER.md", tx));
    let content = match rx.recv().await.unwrap() {
        thunder_tui::event::AppEvent::AgentFinished { final_text, .. } => {
            final_text.unwrap_or_default()
        }
        other => panic!("unexpected event: {other:?}"),
    };
    assert!(
        content.contains("PROJECT_MEMORY_BODY"),
        "show did not read the file:\n{content}"
    );
}

#[tokio::test]
async fn memory_show_rejects_an_unknown_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".thunder")).unwrap();

    let mut app = app_in(dir.path());
    let (tx, mut rx) = mpsc::unbounded_channel();

    assert!(app.execute_slash_command("/memory show nope.md", tx));
    let content = match rx.recv().await.unwrap() {
        thunder_tui::event::AppEvent::AgentFinished { final_text, .. } => {
            final_text.unwrap_or_default()
        }
        other => panic!("unexpected event: {other:?}"),
    };
    assert!(
        content.contains("No memory file matches"),
        "unknown file should be reported:\n{content}"
    );
    let _ = last_assistant(&app);
}
