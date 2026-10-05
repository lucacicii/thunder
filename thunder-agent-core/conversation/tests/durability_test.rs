//! Durability is gated on the first user turn.
//!
//! A host seeds a system prompt and may attach an assistant greeting before the
//! user has typed anything. None of that is a session, so the store must not
//! turn it into a row — on disk, in memory, or in a listing.

use thunder_conversation::prelude::*;

/// The shape `/new` produces: a system prompt plus an assistant greeting, but
/// not a single user turn.
fn greeting_only(id: &str) -> Conversation {
    let mut conv = Conversation::new(id).with_system_prompt("You are a helpful assistant.");
    conv.add_assistant_message(
        Some("✨ Started a fresh conversation session. How can I help you today?".to_string()),
        None,
    );
    conv
}

#[tokio::test]
async fn memory_store_refuses_a_conversation_without_a_user_turn() {
    let store = MemoryConversationStore::new();

    let conv = greeting_only("greeting_only");
    store.save(&conv).await.unwrap();

    assert!(store.load("greeting_only").await.unwrap().is_none());
    assert!(!store.exists("greeting_only").await.unwrap());
    assert!(store
        .list(&ConversationFilter::new())
        .await
        .unwrap()
        .is_empty());

    // The first user turn is the gate, after which the same id is durable.
    let mut spoken = conv;
    spoken.add_user_message("now I am asking something");
    store.save(&spoken).await.unwrap();

    let rows = store.list(&ConversationFilter::new()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turn_count, 1);
}

#[tokio::test]
async fn fs_store_refuses_a_conversation_without_a_user_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsConversationStore::new(dir.path()).await.unwrap();

    let conv = greeting_only("greeting_only");
    store.save(&conv).await.unwrap();

    assert!(store.load("greeting_only").await.unwrap().is_none());
    assert!(!store.exists("greeting_only").await.unwrap());
    assert!(store
        .list(&ConversationFilter::new())
        .await
        .unwrap()
        .is_empty());
    // Not even the directory is created for a conversation nobody spoke to.
    assert!(!dir.path().join("greeting_only").exists());

    let mut spoken = conv;
    spoken.add_user_message("now I am asking something");
    store.save(&spoken).await.unwrap();

    let rows = store.list(&ConversationFilter::new()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turn_count, 1);
    assert!(dir.path().join("greeting_only").exists());
}

#[tokio::test]
async fn min_turns_filter_hides_a_row_a_previous_build_wrote() {
    // Rows written before the store enforced the invariant: present in the
    // index, but with no user turn behind them.
    let mut legacy = Conversation::new("legacy_empty").with_system_prompt("sys");
    legacy.add_assistant_message(Some("hi".to_string()), None);
    let legacy_row = legacy.to_summary();

    let mut spoken = Conversation::new("spoken").with_system_prompt("sys");
    spoken.add_user_message("hello");
    let spoken_row = spoken.to_summary();

    assert_eq!(legacy_row.turn_count, 0);
    assert_eq!(spoken_row.turn_count, 1);

    let open = ConversationFilter::new();
    assert!(
        open.matches(&legacy_row),
        "the default filter keeps every row"
    );
    assert!(open.matches(&spoken_row));

    let listing = ConversationFilter::new().with_min_turns(1);
    assert!(
        !listing.matches(&legacy_row),
        "an empty session is not listable"
    );
    assert!(listing.matches(&spoken_row));
}

#[test]
fn has_user_turns_ignores_system_and_assistant_messages() {
    let mut conv = Conversation::new("gate").with_system_prompt("sys");
    assert!(!conv.has_user_turns());

    conv.add_assistant_message(Some("greeting".to_string()), None);
    assert!(!conv.has_user_turns());

    conv.add_user_message("hello");
    assert!(conv.has_user_turns());
}
