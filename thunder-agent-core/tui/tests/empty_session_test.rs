//! A conversation nobody has spoken to is not a session.
//!
//! The store refuses to persist one, and the listing drops rows that older
//! builds wrote before that rule existed. These tests drive the same `App`
//! paths the TUI uses for a fresh session, its first prompt, and `/clear`.

use std::time::Duration;
use tempfile::tempdir;
use thunder_conversation::prelude::*;
use thunder_tui::app::App;

async fn app_with_store() -> (App, FsConversationStore) {
    let tmp = tempdir().unwrap();
    let store = FsConversationStore::new(tmp.path()).await.unwrap();
    let probe = store.clone();
    (App::new("gpt-4o").with_store(store), probe)
}

#[tokio::test]
async fn a_fresh_session_is_neither_persisted_nor_listed() {
    let (mut app, probe) = app_with_store().await;

    app.new_session();
    let id = app.conversation.id.clone();
    app.save_current_conversation().await;
    app.refresh_sessions().await;

    assert!(
        app.session_list.is_empty(),
        "a session with no user turn must not be listed"
    );
    assert!(
        probe.load(&id).await.unwrap().is_none(),
        "nor written to disk"
    );
    assert!(probe
        .list(&ConversationFilter::new())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_first_user_turn_makes_the_session_durable() {
    let (mut app, probe) = app_with_store().await;

    app.new_session();
    let id = app.conversation.id.clone();

    // The first user turn is the durability gate.
    app.conversation.add_user_message("plan the trip");
    app.save_current_conversation().await;
    app.refresh_sessions().await;

    let ids: Vec<String> = app.session_list.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids, vec![id.clone()]);
    assert_eq!(app.session_list[0].turn_count, 1);
    assert!(probe.load(&id).await.unwrap().is_some());
}

#[tokio::test]
async fn clearing_keeps_the_spoken_session_and_writes_no_greeting_row() {
    let (mut app, probe) = app_with_store().await;

    app.new_session();
    let old_id = app.conversation.id.clone();
    app.conversation.add_user_message("first question");
    app.conversation
        .add_assistant_message(Some("first answer".to_string()), None);
    app.save_current_conversation().await;

    // `/clear` — the greeting assistant message must not turn the new session
    // into a row. A short pause keeps the id (millisecond-derived) distinct.
    tokio::time::sleep(Duration::from_millis(5)).await;
    app.new_session();
    let new_id = app.conversation.id.clone();
    app.conversation.add_assistant_message(
        Some("✨ Started a fresh conversation session. How can I help you today?".to_string()),
        None,
    );
    app.save_and_refresh();
    app.save_current_conversation().await;
    app.refresh_sessions().await;

    assert_ne!(new_id, old_id);
    let ids: Vec<String> = app.session_list.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids, vec![old_id], "only the spoken session survives");
    assert!(probe.load(&new_id).await.unwrap().is_none());
}
