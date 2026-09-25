use crate::plugin::PluginManifest;
use crate::registry::PluginRegistry;
use serde::{Deserialize, Serialize};
use thunder_agent_loop::AgentConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSelection {
    pub active_plugin_ids: Vec<String>,
    pub reason: String,
    pub confidence: f32,
}

#[derive(Clone, Default)]
pub struct PluginSelector {
    pub base_config: Option<AgentConfig>,
}

impl PluginSelector {
    pub fn new(base_config: Option<AgentConfig>) -> Self {
        Self { base_config }
    }

    pub async fn select(
        &self,
        prompt: &str,
        registry: &PluginRegistry,
        _use_mock: bool,
    ) -> PluginSelection {
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

            let matched = m.triggers.keywords.iter().any(|kw| p_lower.contains(kw));
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
