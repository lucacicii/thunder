//! The `web` plugin: Claude Code's `WebFetch`, as a thunder tool.
//!
//! One tool, [`WEB_FETCH_TOOL`], which fetches a URL, converts the page to
//! markdown, and answers a prompt against it with the registry's utility model.
//! When no utility model is configured — or the call fails — the markdown itself
//! is returned, so the capability degrades instead of disappearing.
//!
//! `WebSearch` is deliberately absent: Claude Code implements it as a
//! *server-side* tool (`{"type": "web_search_...}` in the request body), which
//! the transport in `thunder-agent-providers` has no way to pass through, and
//! there is no client-side search backend configured on this machine. A search
//! capability, when it is wanted, belongs behind MCP (`.thunder/mcp.json`).
//!
//! The plugin is registered unconditionally by the standard host assembly, so
//! the TUI and the daemon expose the tool identically. It contributes no system
//! prompt: the tool description carries all the guidance the model needs, and
//! an extra prompt block would only perturb the cached prompt prefix.
pub mod cache;
pub mod convert;
pub mod fetch;
pub mod search;
pub mod summarize;

pub use cache::{global_cache, CachedFetch, FetchCache};
pub use fetch::{WebFetchTool, WEB_FETCH_TOOL};
pub use search::{SearchResult, WebSearchTool, WEB_SEARCH_TOOL};

use crate::plugin::{PluginCapability, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::types::tool::AgentTool;
use thunder_agent_providers::prelude::ProviderRegistry;

/// Contributes `web_fetch` and `web_search`: one reads a page, the other finds
/// them. `web_fetch` is summarised by the registry's utility model.
pub struct WebPlugin {
    manifest: PluginManifest,
    registry: ProviderRegistry,
    /// Shared cache. Defaults to the process-wide one, because hosts rebuild
    /// this plugin (and their root) per request.
    cache: Arc<FetchCache>,
}

impl WebPlugin {
    pub fn new(registry: ProviderRegistry) -> Self {
        let manifest = PluginManifest::new(
            "web",
            "Thunder Web Tools",
            "Fetches a URL, converts the page to markdown, and answers a prompt against it with the utility model; also searches the web through free engines.",
            "0.1.0",
        )
        .with_capability(PluginCapability::ToolProvider)
        // Baseline plugin: the tool must be in the model's tool table from the
        // first turn, not after a keyword happens to match.
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            registry,
            cache: global_cache(),
        }
    }

    /// Use `cache` instead of the process-wide one (tests, isolated hosts).
    pub fn with_cache(mut self, cache: Arc<FetchCache>) -> Self {
        self.cache = cache;
        self
    }
}

#[async_trait]
impl ThunderPlugin for WebPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![
            Arc::new(WebFetchTool::new(
                self.registry.clone(),
                Arc::clone(&self.cache),
            )),
            Arc::new(WebSearchTool::new()),
        ]
    }
}
