use crate::plugin::PluginManifest;
use crate::registry::PluginRegistry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
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
static SELECTION_CACHE: LazyLock<Mutex<HashMap<String, PluginSelection>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Reset the cached selection for a session (e.g. after `reload_plugins`).
pub fn invalidate_session_selection(session_id: &str) {
    SELECTION_CACHE.lock().unwrap().remove(session_id);
}

#[derive(Clone, Default)]
pub struct PluginSelector {
    pub base_config: Option<AgentConfig>,
}

impl PluginSelector {
    pub fn new(base_config: Option<AgentConfig>) -> Self {
        Self { base_config }
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

        if let Some(cached) = SELECTION_CACHE.lock().unwrap().get(session_id) {
            return cached.clone();
        }

        let selection = self.select(prompt, registry).await;
        SELECTION_CACHE
            .lock()
            .unwrap()
            .insert(session_id.to_string(), selection.clone());
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

    pub fn heuristic_select(&self, prompt: &str, manifests: &[PluginManifest]) -> PluginSelection {
        let p_lower = prompt.to_lowercase();
        let mut selected = Vec::new();
        let mut reasons = Vec::new();

        for m in manifests {
            if m.triggers.auto_always {
                selected.push(m.id.clone());
                continue;
            }

            let matched = m
                .triggers
                .keywords
                .iter()
                .any(|kw| keyword_matches(&p_lower, kw));
            if matched {
                selected.push(m.id.clone());
                reasons.push(format!("Matched trigger for plugin '{}'", m.name));
            }
        }

        if selected.is_empty() && manifests.iter().any(|m| m.id == "conversation") {
            selected.push("conversation".to_string());
            reasons.push("Defaulting to conversation plugin".to_string());
        }

        let reason = if reasons.is_empty() {
            "Autonomous heuristic selection based on prompt triggers".to_string()
        } else {
            reasons.join("; ")
        };

        PluginSelection {
            active_plugin_ids: selected,
            reason,
            confidence: 0.90,
        }
    }
}

/// Keyword match with ASCII word boundaries: `"spec"` must not match inside
/// `"specifically"`, `"mcp"` must not match inside `"checksumcp"`. Keywords
/// containing non-ASCII (CJK) fall back to plain substring matching, where
/// word boundaries are not meaningful.
fn keyword_matches(p_lower: &str, kw: &str) -> bool {
    if kw.is_empty() {
        return false;
    }
    if !kw.is_ascii() {
        return p_lower.contains(kw);
    }

    let hay = p_lower.as_bytes();
    let needle = kw.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = p_lower[from..].find(kw) {
        let start = from + rel;
        let end = start + needle.len();
        let boundary_before = start == 0 || !hay[start - 1].is_ascii_alphanumeric();
        let boundary_after = end >= hay.len() || !hay[end].is_ascii_alphanumeric();
        if boundary_before && boundary_after {
            return true;
        }
        from = start + 1;
    }
    false
}
