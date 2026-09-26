//! Tests for the session-selection cache seam.
//!
//! The cache keeps a session's plugin set stable across the per-request
//! `ThunderRoot` rebuilds the hosts perform. These tests pin the two properties
//! that matter: it is *shared* by default (so stability survives a rebuild) and
//! *isolatable* on demand (so two embedded hosts do not cross-wire).

use std::sync::Arc;
use thunder_agent_root::prelude::*;

fn registry_with_conversation() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register(ConversationPlugin::with_memory_store());
    registry
}

#[tokio::test]
async fn default_cache_is_shared_across_selectors() {
    // Two selectors built independently, no cache injected.
    let a = PluginSelector::new(None);
    let b = PluginSelector::new(None);

    // Injecting the *same* session into the default cache must be visible to
    // both, mirroring how the TUI rebuilds its root but keeps one session.
    let cache = default_selection_cache();
    cache.invalidate_all();

    let registry = registry_with_conversation();
    let session = "shared-session-test";
    let first = a
        .select_for_session(Some(session), "hello", &registry)
        .await;
    let second = b
        .select_for_session(Some(session), "hello", &registry)
        .await;

    assert_eq!(first.active_plugin_ids, second.active_plugin_ids);
    cache.invalidate(session);
}

#[tokio::test]
async fn injected_cache_is_isolated() {
    let registry = registry_with_conversation();

    let cache_a = Arc::new(SelectionCache::new());
    let cache_b = Arc::new(SelectionCache::new());

    let selector_a = PluginSelector::new(None).with_cache(Arc::clone(&cache_a));
    let selector_b = PluginSelector::new(None).with_cache(Arc::clone(&cache_b));

    let session = "isolation-session-test";
    selector_a
        .select_for_session(Some(session), "hello", &registry)
        .await;

    // b must not observe a's entry, and querying it must populate only b's cache.
    assert_eq!(cache_b.len(), 0, "isolated cache must not see a's entry");
    selector_b
        .select_for_session(Some(session), "hello", &registry)
        .await;
    assert_eq!(cache_a.len(), 1, "a cached only in its own cache");
    assert_eq!(cache_b.len(), 1, "b cached only in its own cache");
}

#[tokio::test]
async fn invalidate_all_clears_session_lock() {
    let cache = Arc::new(SelectionCache::new());
    let selector = PluginSelector::new(None).with_cache(Arc::clone(&cache));
    let registry = registry_with_conversation();

    selector
        .select_for_session(Some("s1"), "hello", &registry)
        .await;
    selector
        .select_for_session(Some("s2"), "hello", &registry)
        .await;
    assert_eq!(cache.len(), 2);

    cache.invalidate_all();
    assert!(cache.is_empty(), "invalidate_all must clear every session");
}
