//! The `web` plugin: `web_fetch` and `web_search` as thunder tools.
//!
//! Two tools:
//!
//! - [`WEB_FETCH_TOOL`] fetches a URL, converts the page to markdown, and answers
//!   a prompt against it with the registry's utility model. When no utility
//!   model is configured — or the call fails — the markdown itself is returned,
//!   so the capability degrades instead of disappearing.
//! - [`WEB_SEARCH_TOOL`] finds pages: a ranked list of titles, URLs, and
//!   summaries, which the model then reads with `web_fetch`.
//!
//! Each tool has a keyless/local backend and an optional DeepSeek backend
//! (`/v1/tools/search`, `/v1/tools/fetch`). The backend is configuration, not
//! tool arguments: it is read from `.thunder/config.json` (`web.search.provider`
//! / `web.fetch.provider`) once per run in [`ThunderPlugin::on_init`], and the
//! tools are built from that snapshot. The defaults stay on the free engines and
//! the local fetcher, so a key that happens to exist in the environment never
//! starts billing on its own.
//!
//! The plugin is registered unconditionally by the standard host assembly, so
//! the TUI and the daemon expose the tools identically. It contributes no system
//! prompt: the tool descriptions carry all the guidance the model needs, and an
//! extra prompt block would only perturb the cached prompt prefix.
pub mod cache;
pub mod convert;
pub mod deepseek;
pub mod fetch;
pub mod search;
pub mod summarize;

pub use cache::{global_cache, CachedFetch, FetchCache};
pub use deepseek::{DeepSeekClient, DeepSeekConfig, DeepSeekPage};
pub use fetch::{WebFetchTool, WEB_FETCH_TOOL};
pub use search::{SearchProvider, SearchResult, WebSearchTool, WEB_SEARCH_TOOL};

use crate::assembly::{Prepared, PromptSection, RunEnv};
use crate::thunder_config::{ThunderConfig, WebFetchBackend, WebSearchBackend};
use parking_lot::RwLock;
use std::sync::Arc;
use thunder_agent_policy::AgentTool;
use thunder_agent_providers::auth::AuthFile;
use thunder_agent_providers::prelude::ProviderRegistry;

/// Resolved backend choices and credentials for one run.
///
/// The defaults are the pre-DeepSeek behaviour, so a plugin built without
/// `on_init` (tests, embedders that construct the tools directly) is unchanged.
#[derive(Debug, Clone, Default)]
pub struct WebSettings {
    pub search_backend: WebSearchBackend,
    pub fetch_backend: WebFetchBackend,
    /// `None` when no DeepSeek credential resolves; `deepseek` backends then
    /// report the misconfiguration instead of silently downgrading.
    pub deepseek: Option<deepseek::DeepSeekConfig>,
    /// The user's turn for this run, as DeepSeek's `ori_question`: the index
    /// infers intent better from the sentence a query was derived from than
    /// from the query alone. Captured in `on_init`, which is the only place a
    /// plugin sees the user's input.
    pub ori_question: Option<String>,
}

impl WebSettings {
    /// Resolve settings from a merged config document, the provider registry,
    /// and the user's credentials.
    pub fn resolve(
        config: &ThunderConfig,
        registry: &ProviderRegistry,
        auth: &AuthFile,
        prompt: Option<&str>,
    ) -> Self {
        Self {
            search_backend: config.search_backend(),
            fetch_backend: config.fetch_backend(),
            deepseek: deepseek::resolve(config.deepseek(), registry, auth),
            ori_question: prompt.map(str::to_string),
        }
    }
}

/// Contributes `web_fetch` and `web_search`: one reads a page, the other finds
/// them. `web_fetch` is summarised by the registry's utility model.
pub struct WebPlugin {
    registry: ProviderRegistry,
    /// Shared cache. Defaults to the process-wide one, because hosts rebuild
    /// this plugin (and their root) per request.
    cache: Arc<FetchCache>,
    /// Backend selection, filled by `on_init` (which runs before `tools()`).
    /// Behind a lock because the plugin instance is shared across a run's
    /// lifecycle while `tools()` takes `&self`.
    settings: RwLock<WebSettings>,
}

impl WebPlugin {
    pub const ID: &'static str = "web";

    pub fn new(registry: ProviderRegistry) -> Self {
        Self {
            registry,
            cache: global_cache(),
            settings: RwLock::new(WebSettings::default()),
        }
    }

    /// Use `cache` instead of the process-wide one (tests, isolated hosts).
    pub fn with_cache(mut self, cache: Arc<FetchCache>) -> Self {
        self.cache = cache;
        self
    }

    /// Pre-seed the backend settings, skipping the `on_init` config read.
    /// Tests and embedders that already know their settings use this.
    pub fn with_settings(self, settings: WebSettings) -> Self {
        *self.settings.write() = settings;
        self
    }

    /// The settings `tools()` will build from.
    pub fn settings(&self) -> WebSettings {
        self.settings.read().clone()
    }
}

impl PromptSection for WebPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn render(&self) -> Option<String> {
        None
    }
}

impl WebPlugin {
    /// The tools this run may call, built from the settings `prepare` resolved.
    pub fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let settings = self.settings();
        let client = settings.deepseek.clone().map(deepseek::DeepSeekClient::new);
        vec![
            Arc::new(
                WebFetchTool::new(self.registry.clone(), Arc::clone(&self.cache))
                    .with_deepseek(client.clone())
                    .with_backend(settings.fetch_backend),
            ),
            Arc::new(
                WebSearchTool::new()
                    .with_deepseek(client)
                    .with_backend(settings.search_backend)
                    .with_ori_question(settings.ori_question),
            ),
        ]
    }
}

impl Prepared for WebPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn prepare<'a>(
        &'a self,
        env: &'a RunEnv,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let config = ThunderConfig::load(env.workspace.as_deref()).await;
            let auth = AuthFile::load_default().await;
            let resolved =
                WebSettings::resolve(&config, &self.registry, &auth, env.prompt.as_deref());
            tracing::debug!(
                search = ?resolved.search_backend,
                fetch = ?resolved.fetch_backend,
                deepseek = resolved.deepseek.is_some(),
                "Resolved web tool backends"
            );
            *self.settings.write() = resolved;
            Ok(())
        })
    }
}
