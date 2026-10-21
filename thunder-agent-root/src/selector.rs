use crate::plugin::PluginManifest;
use crate::registry::PluginRegistry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use thunder_agent_loop::AgentConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSelection {
    pub active_plugin_ids: Vec<String>,
    pub reason: String,
    pub confidence: f32,
}

/// Session-scoped selection cache: the FIRST message of a session decides the
/// active plugin set for the whole session. Re-selecting per message made the
/// combined system prompt (position 0) and the registered toolset drift between
/// turns, invalidating the provider-side prompt cache for every request.
/// Forced selections bypass the cache entirely.
///
/// The cache is `Arc`-shared rather than a bare global so a host can either
/// reuse the **process-wide default** (what TUI and daemon do — both rebuild
/// their `ThunderRoot` per request, so the selection must outlive any single
/// root) or inject an **isolated** one, which keeps two hosts embedded in the
/// same process from cross-wiring on a colliding session id.
#[derive(Debug, Default)]
pub struct SelectionCache {
    map: Mutex<HashMap<String, PluginSelection>>,
}

impl SelectionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, session_id: &str) -> Option<PluginSelection> {
        self.map.lock().unwrap().get(session_id).cloned()
    }

    pub fn insert(&self, session_id: &str, selection: PluginSelection) {
        self.map
            .lock()
            .unwrap()
            .insert(session_id.to_string(), selection);
    }

    /// Drop one session's cached selection (e.g. after `reload_plugins`).
    pub fn invalidate(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
    }

    /// Drop every cached selection. Use after a registry-wide change (e.g. a
    /// plugin reload) where any session could have cached a now-stale set.
    pub fn invalidate_all(&self) {
        self.map.lock().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.lock().unwrap().is_empty()
    }
}

static DEFAULT_SELECTION_CACHE: LazyLock<Arc<SelectionCache>> =
    LazyLock::new(|| Arc::new(SelectionCache::new()));

/// The process-wide default cache. Roots that do not inject their own share it,
/// which is what keeps a session's plugin set stable across the per-request
/// `ThunderRoot` rebuilds that the TUI and daemon both perform.
pub fn default_selection_cache() -> Arc<SelectionCache> {
    Arc::clone(&DEFAULT_SELECTION_CACHE)
}

/// Reset the cached selection for a session on the default cache.
pub fn invalidate_session_selection(session_id: &str) {
    DEFAULT_SELECTION_CACHE.invalidate(session_id);
}

/// Reset every cached selection on the default cache.
pub fn invalidate_all_session_selections() {
    DEFAULT_SELECTION_CACHE.invalidate_all();
}

#[derive(Clone, Default)]
pub struct PluginSelector {
    pub base_config: Option<AgentConfig>,
    cache: Option<Arc<SelectionCache>>,
}

impl PluginSelector {
    pub fn new(base_config: Option<AgentConfig>) -> Self {
        Self {
            base_config,
            cache: None,
        }
    }

    /// Use a specific cache instead of the process-wide default.
    pub fn with_cache(mut self, cache: Arc<SelectionCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// The cache backing this selector (defaulting to the process-wide one).
    pub fn cache(&self) -> Arc<SelectionCache> {
        self.cache.clone().unwrap_or_else(default_selection_cache)
    }

    /// Select the active plugin set for a session. When a session id is
    /// provided the selection is computed once and cached for the session
    /// lifetime, keeping the request prefix stable across turns.
    pub async fn select_for_session(
        &self,
        session_id: Option<&str>,
        prompt: &str,
        registry: &PluginRegistry,
    ) -> PluginSelection {
        let Some(session_id) = session_id else {
            return self.select(prompt, registry).await;
        };

        let cache = self.cache();
        if let Some(cached) = cache.get(session_id) {
            return cached;
        }

        let selection = self.select(prompt, registry).await;
        cache.insert(session_id, selection.clone());
        selection
    }

    pub async fn select(&self, prompt: &str, registry: &PluginRegistry) -> PluginSelection {
        let manifests = registry.list_manifests();
        if manifests.is_empty() {
            return PluginSelection {
                active_plugin_ids: Vec::new(),
                reason: "No plugins registered in registry.".to_string(),
                confidence: 1.0,
            };
        }

        // Fast deterministic trigger and keyword matching: avoids adding 1-2s of LLM latency
        // and extra token cost on every task initiation.
        self.heuristic_select(prompt, &manifests)
    }

    /// Every registered plugin is active.
    ///
    /// This used to match keywords in the prompt. That heuristic was fragile
    /// (one word changed the toolset), invisible (nothing told the model the set
    /// had changed) and cache-hostile (the prompt prefix moved with it). The
    /// session-locked cache keeps the set stable across turns; the set itself is
    /// now "everything the host registered".
    pub fn heuristic_select(&self, _prompt: &str, manifests: &[PluginManifest]) -> PluginSelection {
        PluginSelection {
            active_plugin_ids: manifests.iter().map(|m| m.id.clone()).collect(),
            reason: "All registered plugins are active".to_string(),
            confidence: 1.0,
        }
    }
}
