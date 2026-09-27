//! Canonical host plugin assembly.
//!
//! Every front-end (TUI, daemon, embedders) should build its [`ThunderRoot`]
//! through [`StandardHostBuilder`] instead of hand-wiring plugins.
//!
//! Centralising the assembly is what keeps **capability parity** across hosts.
//! Previously the TUI and the daemon each registered their own subset by hand,
//! and had already drifted: the TUI silently shipped without the TypeScript
//! plugin host, so a capability that existed in the daemon was simply absent
//! from the terminal app.
//!
//! The baseline set is:
//!
//! - `conversation` — always (caller supplies the store: memory or FS)
//! - `skills`       — always, unless the caller supplies a preloaded one
//! - `mcp`          — always (no-ops when the workspace configures no servers)
//! - `script_plugin` — **opt-in**, because it spawns a Node sidecar; a host
//!   that wants single-file TypeScript plugins declares so explicitly rather
//!   than getting them by accident.
//!
//! Host-specific capabilities that need host-owned resources (e.g. the daemon's
//! `ask_user` plugin, which needs its NDJSON writer) are appended after
//! [`StandardHostBuilder::build`] via `ThunderRoot::with_plugin`.

use crate::host::ThunderRoot;
#[cfg(feature = "conversation")]
use crate::plugins::ConversationPlugin;
#[cfg(feature = "script-plugin")]
use crate::plugins::ScriptPlugin;
#[cfg(feature = "skills")]
use crate::plugins::SkillsPlugin;
#[cfg(feature = "conversation")]
use std::sync::Arc;
#[cfg(feature = "conversation")]
use thunder_conversation::prelude::ConversationStore;

/// Assembles the canonical baseline plugin set onto a [`ThunderRoot`].
///
/// Call [`ThunderRoot::with_workspace`] / `with_extra_roots` /
/// `with_provider_registry` **before** `build`, so the plugins and the agent
/// config see the final workspace.
#[cfg(feature = "conversation")]
pub struct StandardHostBuilder {
    store: Arc<dyn ConversationStore>,
    #[cfg(feature = "skills")]
    skills: Option<SkillsPlugin>,
    #[cfg(feature = "script-plugin")]
    script: Option<ScriptPlugin>,
}

#[cfg(feature = "conversation")]
impl StandardHostBuilder {
    /// Start from the standard set with `store` backing the conversation plugin.
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self {
            store,
            #[cfg(feature = "skills")]
            skills: None,
            #[cfg(feature = "script-plugin")]
            script: None,
        }
    }

    /// Use `skills` instead of the default `SkillsPlugin` (e.g. to preload an
    /// active skill or add search paths).
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, skills: SkillsPlugin) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Enable the single-file TypeScript plugin host.
    ///
    /// Pass a ready-made [`ScriptPlugin`] so a host that also drives
    /// `reload` keeps sharing the same sidecar handle; the plugin's internal
    /// locks are `Arc`-backed, so clones refer to one engine.
    #[cfg(feature = "script-plugin")]
    pub fn with_script_plugin(mut self, script: ScriptPlugin) -> Self {
        self.script = Some(script);
        self
    }

    /// Register the assembled set onto `root`.
    pub fn build(self, mut root: ThunderRoot) -> ThunderRoot {
        root = root.with_plugin(ConversationPlugin::new(self.store));

        #[cfg(feature = "skills")]
        {
            root = root.with_plugin(self.skills.unwrap_or_default());
        }

        #[cfg(feature = "mcp")]
        {
            root = root.with_plugin(crate::plugins::McpPlugin::default());
        }

        #[cfg(feature = "script-plugin")]
        {
            if let Some(script) = self.script {
                root = root.with_plugin(script);
            }
        }

        root
    }
}

/// Convenience: the plugin IDs a host should force-active for a normal run.
///
/// Mirrors the assembly above so the *forced* set and the *registered* set
/// cannot drift apart. Two plugins are conditional:
///
/// * `mcp` — only when the workspace actually configures servers;
/// * `script_plugin` — only when a loadable plugin file exists.
///
/// Both are opt-in by design rather than an oversight. Selecting a plugin
/// dispatches `on_init`, and the script host spawns a Node process on first use,
/// so forcing it unconditionally would tax every user who has no plugins. The
/// flip side is worse: a user who *has* plugins must not silently not get them.
#[cfg(feature = "conversation")]
pub fn baseline_forced_plugins(
    workspace_has_mcp_config: bool,
    workspace_has_ts_plugins: bool,
) -> Vec<String> {
    let mut ids = vec!["conversation".to_string()];
    #[cfg(feature = "skills")]
    {
        ids.push("skills".to_string());
    }
    if workspace_has_mcp_config {
        ids.push("mcp".to_string());
    }
    if workspace_has_ts_plugins {
        ids.push("script_plugin".to_string());
    }
    ids
}

/// Whether any single-file plugin exists to load.
///
/// Global scope (`~/.thunder/plugins`) plus the workspace scope, mirroring the
/// sidecar's own discovery order. Two directory reads, cheap enough per run.
pub fn has_ts_plugins(workspace_dir: Option<&std::path::Path>) -> bool {
    fn any_plugin(dir: std::path::PathBuf) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|e| {
            e.file_type().map(|t| t.is_file()).unwrap_or(false)
                && matches!(
                    e.path().extension().and_then(|x| x.to_str()),
                    Some("ts") | Some("js")
                )
        })
    }

    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        if any_plugin(home.join(".thunder").join("plugins")) {
            return true;
        }
    }
    workspace_dir.is_some_and(|ws| any_plugin(ws.join(".arp").join("plugins")))
}
