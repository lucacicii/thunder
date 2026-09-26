//! Regression guard for cross-host plugin parity.
//!
//! The TUI and the daemon used to hand-wire their own plugin lists, and had
//! already drifted (the TUI shipped without the TypeScript plugin host). These
//! tests pin the *shared* assembly so a future host that forgets a baseline
//! plugin fails here rather than silently losing a capability at runtime.

#![cfg(feature = "conversation")]

use std::sync::Arc;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::MemoryConversationStore;

fn baseline_root() -> ThunderRoot {
    StandardHostBuilder::new(Arc::new(MemoryConversationStore::new())).build(ThunderRoot::new(
        thunder_agent_loop::prelude::AgentConfig::new("test/model"),
    ))
}

#[test]
fn standard_host_registers_conversation_and_skills() {
    let root = baseline_root();
    let ids: Vec<String> = root
        .registry()
        .list_manifests()
        .iter()
        .map(|m| m.id.clone())
        .collect();

    assert!(
        ids.contains(&"conversation".to_string()),
        "conversation plugin missing: {ids:?}"
    );
    assert!(
        ids.contains(&"skills".to_string()),
        "skills plugin missing: {ids:?}"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn standard_host_registers_mcp() {
    let root = baseline_root();
    let ids: Vec<String> = root
        .registry()
        .list_manifests()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert!(
        ids.contains(&"mcp".to_string()),
        "mcp plugin missing: {ids:?}"
    );
}

/// The script host is opt-in: it must NOT appear unless explicitly requested,
/// because it spawns a Node sidecar.
#[cfg(feature = "script-plugin")]
#[test]
fn script_plugin_is_opt_in() {
    let root = baseline_root();
    let ids: Vec<String> = root
        .registry()
        .list_manifests()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert!(
        !ids.contains(&"script_plugin".to_string()),
        "script host must stay opt-in, but was registered: {ids:?}"
    );
}

#[test]
fn baseline_forced_plugins_track_registration() {
    // Without MCP config the forced set is conversation + skills.
    let ids = baseline_forced_plugins(false);
    assert!(ids.contains(&"conversation".to_string()));
    assert!(!ids.contains(&"mcp".to_string()));

    // With MCP config, mcp joins the forced set.
    let ids = baseline_forced_plugins(true);
    assert!(ids.contains(&"mcp".to_string()));
}
