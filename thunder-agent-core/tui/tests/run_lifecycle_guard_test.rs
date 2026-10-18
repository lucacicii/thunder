//! Run/session lifecycle guards: a live run owns the transcript, and a result
//! from a superseded run must not rewrite it.

use thunder_agent_loop::types::event::AgentStats;
use thunder_agent_loop::types::message::ChatMessage;
use thunder_conversation::prelude::*;
use thunder_tui::app::App;
use tokio_util::sync::CancellationToken;

fn live_run(app: &mut App, run_id: &str) {
    app.cancel_token = Some(CancellationToken::new());
    app.active_run_id = Some(run_id.to_string());
}

#[tokio::test]
async fn new_session_is_refused_while_a_run_is_live() {
    let mut app = App::new("gpt-4o");
    let original = app.conversation.id.clone();
    live_run(&mut app, "run_1");

    assert!(!app.new_session(), "the switch must report refusal");
    assert_eq!(
        app.conversation.id, original,
        "the transcript the run is writing into must survive"
    );
    assert_eq!(
        app.active_run_id.as_deref(),
        Some("run_1"),
        "the run keeps its identity"
    );
}

#[tokio::test]
async fn new_session_works_once_the_run_is_gone() {
    let mut app = App::new("gpt-4o");
    let original = app.conversation.id.clone();

    assert!(app.new_session());
    assert_ne!(app.conversation.id, original);
}

#[tokio::test]
async fn load_conversation_is_refused_while_a_run_is_live() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FsConversationStore::new(tmp.path()).await.unwrap();
    let mut app = App::new("gpt-4o").with_store(store);
    let original = app.conversation.id.clone();
    live_run(&mut app, "run_1");

    app.load_conversation("some_other_session").await;

    assert_eq!(
        app.conversation.id, original,
        "loading over a live run would splice two sessions together"
    );
}

#[tokio::test]
async fn a_superseded_run_result_is_ignored() {
    let mut app = App::new("gpt-4o");
    app.conversation.add_user_message("first".to_string());
    app.conversation
        .add_assistant_message(Some("current answer".to_string()), None);
    let current = app.conversation.messages.clone();
    live_run(&mut app, "run_2");

    // The run the user stopped settles after run_2 took over.
    app.handle_agent_finished(
        Some("run_1".to_string()),
        "tui_agent".to_string(),
        true,
        Some("stale answer".to_string()),
        Some(vec![
            ChatMessage::user("first"),
            ChatMessage::assistant_text("stale answer"),
        ]),
        None,
        None,
        Some("Done".to_string()),
    );

    assert_eq!(
        app.conversation.messages, current,
        "the stale run must not replace the live transcript"
    );
    assert_eq!(
        app.active_run_id.as_deref(),
        Some("run_2"),
        "and must not clear the live run's identity"
    );
    assert!(app.cancel_token.is_some(), "and must not end the live run");
}

#[tokio::test]
async fn a_matching_run_result_is_applied() {
    let mut app = App::new("gpt-4o");
    live_run(&mut app, "run_2");

    app.handle_agent_finished(
        Some("run_2".to_string()),
        "tui_agent".to_string(),
        true,
        Some("done".to_string()),
        Some(vec![
            ChatMessage::user("go"),
            ChatMessage::assistant_text("done"),
        ]),
        None,
        None,
        Some("Done".to_string()),
    );

    assert_eq!(app.conversation.messages.len(), 2);
    assert_eq!(app.conversation.messages[1].content_str(), Some("done"));
    assert!(app.cancel_token.is_none());
}

#[tokio::test]
async fn a_budget_stop_keeps_the_authoritative_transcript() {
    let mut app = App::new("gpt-4o");
    live_run(&mut app, "run_1");
    let stats = AgentStats::default();

    // Three turns of real work the host's event view never fully assembled.
    app.handle_agent_finished(
        Some("run_1".to_string()),
        "tui_agent".to_string(),
        false,
        Some("partial answer".to_string()),
        Some(vec![
            ChatMessage::user("go"),
            ChatMessage::assistant_text("partial answer"),
            ChatMessage::tool("call_1", "file contents", Some("read_file".to_string())),
        ]),
        None,
        Some(stats),
        Some("MaxTurnsExceeded".to_string()),
    );

    assert!(
        app.conversation
            .messages
            .iter()
            .any(|m| matches!(m, ChatMessage::Tool { .. })),
        "the engine's tool results must survive a turn-budget stop: {:?}",
        app.conversation.messages
    );
    assert_eq!(
        app.agent_status,
        thunder_tui::prelude::AgentStatus::Idle,
        "a budget stop is a continuation point, not an error"
    );
}
