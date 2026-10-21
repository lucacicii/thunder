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
//! - `memory`       — always (no-ops when no `THUNDER.md` / `memory/*.md`)
//! - `skills`       — always, unless the caller supplies a preloaded one
//! - `mcp`          — always (no-ops when the workspace configures no servers)
//! - `metrics`      — always, unless the caller supplies a shared one
//! - `agent_status` — always, unless the caller supplies a shared one
//!
//! The TypeScript plugin host used to be the fifth member, opt-in because it
//! spawned a Node sidecar. It is gone: plugins are Rust now, so there is no
//! sidecar to opt into and nothing to force-active.
//!
//! Host-specific capabilities that need host-owned resources (e.g. the daemon's
//! `ask_user` plugin, which needs its NDJSON writer) are appended after
//! [`StandardHostBuilder::build`] via `ThunderRoot::with_plugin`.

use crate::host::ThunderRoot;
#[cfg(feature = "conversation")]
use crate::plugins::ConversationPlugin;
#[cfg(feature = "conversation")]
use crate::plugins::MemoryPlugin;
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
    #[cfg(feature = "mcp")]
    mcp: Option<crate::plugins::McpPlugin>,
    metrics: Option<crate::plugins::MetricsPlugin>,
    status: Option<crate::plugins::StatusPlugin>,
}

#[cfg(feature = "conversation")]
impl StandardHostBuilder {
    /// Start from the standard set with `store` backing the conversation plugin.
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        Self {
            store,
            #[cfg(feature = "skills")]
            skills: None,
            #[cfg(feature = "mcp")]
            mcp: None,
            metrics: None,
            status: None,
        }
    }

    /// Use `metrics` instead of a fresh `MetricsPlugin` (e.g. to share a
    /// session-wide telemetry tracker across runs).
    pub fn with_metrics_plugin(mut self, metrics: crate::plugins::MetricsPlugin) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Use `status` instead of a fresh [`StatusPlugin`](crate::plugins::StatusPlugin).
    ///
    /// A host passes its own so it can *hold* the status it publishes: the
    /// dialog path reports `awaiting_user` through that same handle.
    pub fn with_status_plugin(mut self, status: crate::plugins::StatusPlugin) -> Self {
        self.status = Some(status);
        self
    }
    /// Use `skills` instead of the default `SkillsPlugin` (e.g. to preload an
    /// active skill or add search paths).
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, skills: SkillsPlugin) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Use `mcp` instead of the default `McpPlugin` (e.g. to share an MCP client pool).
    #[cfg(feature = "mcp")]
    pub fn with_mcp_plugin(mut self, mcp: crate::plugins::McpPlugin) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Register the assembled set onto `root`.
    pub fn build(self, mut root: ThunderRoot) -> ThunderRoot {
        let conversation = Arc::new(ConversationPlugin::new(self.store));
        root = root
            .with_prepared(Arc::clone(&conversation) as Arc<dyn crate::assembly::Prepared>)
            .with_section(Arc::clone(&conversation) as Arc<dyn crate::assembly::PromptSection>)
            .with_observer(conversation as Arc<dyn crate::assembly::Observer>);
        root = root.with_metrics_plugin(self.metrics.unwrap_or_default());
        // What the agent is doing right now, on the same footing as the numbers
        // above: a host that wants to *report* the status (TUI pane title, panel
        // chip, a dialog-driven `awaiting_user`) needs this very instance.
        root = root.with_status_plugin(self.status.unwrap_or_default());
        // Long-term memory rides the baseline set: it contributes nothing when
        // no `THUNDER.md` exists, so every host gets it for free.
        let memory = Arc::new(MemoryPlugin::new());
        root = root
            .with_prepared(Arc::clone(&memory) as Arc<dyn crate::assembly::Prepared>)
            .with_section(Arc::clone(&memory) as Arc<dyn crate::assembly::PromptSection>)
            .with_pack(crate::assembly::ToolPack::new(MemoryPlugin::ID).with_tools(memory.tools()));

        {
            // The provider registry carries the utility model `web_fetch`
            // summarises with, so the plugin is built from the root the caller
            // already configured. `with_provider_registry` must run before
            // `build` — the doc comment on `StandardHostBuilder` says so — or
            // the tool silently loses its summariser and returns raw markdown.
            let registry = root.provider_registry().clone();
            let web = Arc::new(crate::plugins::web::WebPlugin::new(registry));
            let web_tools = web.tools();
            root = root
                .with_prepared(Arc::clone(&web) as Arc<dyn crate::assembly::Prepared>)
                .with_pack(
                    crate::assembly::ToolPack::new(crate::plugins::web::WebPlugin::ID)
                        .with_tools(web_tools),
                );
        }

        #[cfg(feature = "skills")]
        {
            let skills = Arc::new(self.skills.unwrap_or_default());
            let tools = skills.tools();
            root = root
                .with_prepared(Arc::clone(&skills) as Arc<dyn crate::assembly::Prepared>)
                .with_section(Arc::clone(&skills) as Arc<dyn crate::assembly::PromptSection>)
                .with_pack(crate::assembly::ToolPack::new(SkillsPlugin::ID).with_tools(tools));
        }

        #[cfg(feature = "mcp")]
        {
            let mcp = Arc::new(self.mcp.unwrap_or_default());
            let tools = mcp.tools();
            root = root
                .with_prepared(Arc::clone(&mcp) as Arc<dyn crate::assembly::Prepared>)
                .with_section(Arc::clone(&mcp) as Arc<dyn crate::assembly::PromptSection>)
                .with_pack(
                    crate::assembly::ToolPack::new(crate::plugins::mcp::McpPlugin::ID)
                        .with_tools(tools),
                );
        }

        root
    }
}
