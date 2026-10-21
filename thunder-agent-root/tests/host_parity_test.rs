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
    let ids = root.component_ids();

    assert!(
        ids.contains(&"conversation".to_string()),
        "conversation plugin missing: {ids:?}"
    );
    assert!(
        ids.contains(&"skills".to_string()),
        "skills plugin missing: {ids:?}"
    );
}

#[test]
fn standard_host_registers_web() {
    let root = baseline_root();
    let ids = root.component_ids();
    assert!(
        ids.contains(&"web".to_string()),
        "web plugin missing: {ids:?}"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn standard_host_registers_mcp() {
    let root = baseline_root();
    let ids = root.component_ids();
    assert!(
        ids.contains(&"mcp".to_string()),
        "mcp plugin missing: {ids:?}"
    );
}
