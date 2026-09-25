//! Inline pi-ai `Model` definition sent from Rust to the bridge sidecar.
//!
//! This mirrors the fields pi-ai accepts on an ad-hoc (unregistered) model:
//! compat is auto-detected from `base_url` unless explicitly overridden via
//! `compat`. Keeping this DTO inside the bridge crate (instead of reusing
//! `ModelSpec` from thunder-agent-providers) keeps the dependency arrow
//! one-way: providers → bridge.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A pi-ai API identifier string, e.g. `"openai-completions"`.
pub const API_OPENAI_COMPLETIONS: &str = "openai-completions";
pub const API_OPENAI_RESPONSES: &str = "openai-responses";
pub const API_ANTHROPIC_MESSAGES: &str = "anthropic-messages";
pub const API_GOOGLE_GENERATIVE_AI: &str = "google-generative-ai";

/// Per-million-token pricing (pi `Model.cost`). Thunder defaults to zeros;
/// pi-ai's `calculateCost` reads this unconditionally.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BridgeCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    #[serde(default)]
    pub tiers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BridgeModel {
    /// pi provider id (thunder provider id, used for routing/replies only)
    pub provider: String,
    /// Bare model id on the wire (never `provider/model`)
    pub id: String,
    pub name: String,
    /// pi Api string; see the `API_*` constants
    pub api: String,
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub reasoning: bool,
    /// pi `Model.input`: content modalities, e.g. `["text"]` or `["text","image"]`.
    /// pi-ai requires it (image downgrade logic reads it); thunder is text-only
    /// by default, so an empty vec is normalized to `["text"]` by the sidecar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    #[serde(default)]
    pub context_window: usize,
    #[serde(default)]
    pub max_tokens: usize,
    /// pi `thinkingLevelMap`: thunder level → provider-native level.
    /// `null` values mark unsupported levels (same semantics as pi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    /// pi compat overrides passthrough (OpenAICompletionsCompat-shaped JSON)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<serde_json::Value>,
    /// Pricing info; `None` is normalized to zeros by the sidecar (pi-ai
    /// requires the field for usage cost accounting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<BridgeCost>,
}

impl BridgeModel {
    pub fn new(provider: impl Into<String>, id: impl Into<String>, api: impl Into<String>) -> Self {
        let id: String = id.into();
        Self {
            provider: provider.into(),
            name: id.clone(),
            id,
            api: api.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_camel_case_for_pi_model_shape() {
        let mut model = BridgeModel::new("cc-switch", "deepseek/v4-flash", API_OPENAI_COMPLETIONS);
        model.base_url = "https://api.example.com/v1".into();
        model.api_key = Some("sk-test".into());
        model.context_window = 1_000_000;
        model.max_tokens = 16_384;
        let mut map = HashMap::new();
        map.insert("high".to_string(), Some("max".to_string()));
        map.insert("medium".to_string(), None);
        model.thinking_level_map = Some(map);

        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(json["baseUrl"], "https://api.example.com/v1");
        assert_eq!(json["apiKey"], "sk-test");
        assert_eq!(json["contextWindow"], 1_000_000);
        assert_eq!(json["maxTokens"], 16_384);
        assert_eq!(json["thinkingLevelMap"]["high"], "max");
        assert!(json["thinkingLevelMap"]["medium"].is_null());
        // OpenAI-shaped names must never leak in
        assert!(json.get("base_url").is_none());
        assert!(json.get("context_window").is_none());
    }
}
