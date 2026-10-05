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
    // Nothing configured: conversation + skills only, and no Node sidecar.
    let ids = baseline_forced_plugins(false, false);
    assert!(ids.contains(&"conversation".to_string()));
    assert!(!ids.contains(&"mcp".to_string()));
    assert!(
        !ids.contains(&"script_plugin".to_string()),
        "a workspace with no plugin files must not boot the sidecar"
    );

    // With MCP config, mcp joins the forced set.
    let ids = baseline_forced_plugins(true, false);
    assert!(ids.contains(&"mcp".to_string()));
    assert!(!ids.contains(&"script_plugin".to_string()));

    // With plugin files present, the script host must actually be reachable —
    // the failure this guards against is a user writing a plugin and silently
    // not getting it.
    let ids = baseline_forced_plugins(false, true);
    assert!(ids.contains(&"script_plugin".to_string()));
}

#[test]
fn ts_plugin_detection_looks_in_both_scopes() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let plugins = ws.join(".thunder").join("plugins");
    assert!(!has_ts_plugins(Some(ws)), "no directory yet");

    std::fs::create_dir_all(&plugins).unwrap();
    assert!(!has_ts_plugins(Some(ws)), "empty directory");

    std::fs::write(plugins.join("notes.md"), "not a plugin").unwrap();
    assert!(
        !has_ts_plugins(Some(ws)),
        "only .ts/.js files count as plugins"
    );

    std::fs::write(plugins.join("demo.ts"), "export default definePlugin({});").unwrap();
    assert!(has_ts_plugins(Some(ws)), "a .ts file is a plugin");

    assert!(!has_ts_plugins(None), "no workspace scope is not an error");
}
