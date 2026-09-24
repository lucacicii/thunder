use crate::api::ProviderApi;
use crate::error::ProviderError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsFile {
    #[serde(
        default,
        alias = "utility_model",
        alias = "utilityModel",
        alias = "tool_model",
        alias = "toolModel",
        alias = "fast_model",
        alias = "fastModel",
        alias = "naming_model",
        alias = "namingModel"
    )]
    pub utility_model: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderFileConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFileConfig {
    pub base_url: Option<String>,
    pub api: Option<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub models: Vec<ModelFileConfig>,
    pub compat: Option<CompatConfig>,
    #[serde(alias = "thinking_levels")]
    pub thinking_levels: Option<Vec<String>>,
    #[serde(alias = "default_thinking_level")]
    pub default_thinking_level: Option<String>,
    #[serde(default, alias = "thinking_level_map", alias = "thinkingLevelMap")]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFileConfig {
    pub id: String,
    pub name: Option<String>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    pub context_window: Option<usize>,
    pub max_tokens: Option<usize>,
    pub compat: Option<CompatConfig>,
    #[serde(alias = "thinking_levels")]
    pub thinking_levels: Option<Vec<String>>,
    #[serde(alias = "default_thinking_level")]
    pub default_thinking_level: Option<String>,
    #[serde(default, alias = "thinking_level_map", alias = "thinkingLevelMap")]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    #[serde(default, alias = "thinking_levels_probed", alias = "thinkingLevelsProbed", alias = "probed")]
    pub thinking_levels_probed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatConfig {
    pub supports_developer_role: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub max_tokens_field: Option<String>,
}

pub fn sort_thinking_levels(levels: &[String]) -> Vec<String> {
    const CANONICAL_ORDER: &[&str] = &[
        "off", "none", "minimal", "low", "medium", "high", "xhigh", "max",
    ];
    let mut sorted = levels.to_vec();
    sorted.sort_by_key(|level| {
        let l = level.to_lowercase();
        CANONICAL_ORDER
            .iter()
            .position(|&c| c == l)
            .unwrap_or(CANONICAL_ORDER.len() + 1)
    });
    sorted.dedup();
    sorted
}

pub fn extract_levels_from_map(map: &HashMap<String, Option<String>>) -> Vec<String> {
    let mut raw_levels = Vec::new();
    for (k, v) in map {
        if v.is_some() {
            raw_levels.push(k.clone());
        }
    }
    sort_thinking_levels(&raw_levels)
}

impl ModelsFile {
    pub fn parse_json(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| ProviderError::Config(e.to_string()))
    }

    pub async fn load_path(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let raw = tokio::fs::read_to_string(path.as_ref())
            .await
            .map_err(|e| ProviderError::Config(format!("{}: {e}", path.as_ref().display())))?;
        Self::parse_json(&raw)
    }

    pub fn default_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from(".thunder/models.json")];
        if let Ok(dir) = std::env::var("THUNDER_CONFIG_DIR") {
            paths.push(PathBuf::from(dir).join("models.json"));
        }
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".thunder/models.json"));
        }
        paths
    }

    pub fn primary_config_path() -> Option<PathBuf> {
        for path in Self::default_paths() {
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            Some(PathBuf::from(home).join(".thunder/models.json"))
        } else {
            None
        }
    }

    pub async fn load_default() -> Self {
        for path in Self::default_paths() {
            if path.exists() {
                if let Ok(parsed) = Self::load_path(path).await {
                    return parsed;
                }
            }
        }
        Self::default()
    }

    /// Persist probe result to disk and set thinkingLevelsProbed flag to true.
    pub async fn update_model_thinking_probed_config(
        provider_id: &str,
        model_id: &str,
        thinking_levels: &[String],
        default_thinking_level: &str,
    ) -> Result<PathBuf, ProviderError> {
        let config_path = Self::primary_config_path().ok_or_else(|| {
            ProviderError::Config("Cannot locate primary models.json configuration path".to_string())
        })?;

        let raw = if config_path.exists() {
            tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| ProviderError::Config(format!("{}: {e}", config_path.display())))?
        } else {
            "{}".to_string()
        };

        let mut root: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Config(format!("Failed to parse {}: {e}", config_path.display())))?;

        let mut updated = false;

        if let Some(providers) = root.get_mut("providers").and_then(|p| p.as_object_mut()) {
            if let Some(prov_val) = providers.get_mut(provider_id) {
                if let Some(models) = prov_val.get_mut("models").and_then(|m| m.as_array_mut()) {
                    for m in models.iter_mut() {
                        if m.get("id").and_then(|v| v.as_str()) == Some(model_id) {
                            if let Some(obj) = m.as_object_mut() {
                                obj.insert("thinkingLevels".to_string(), serde_json::json!(thinking_levels));
                                obj.insert("defaultThinkingLevel".to_string(), serde_json::json!(default_thinking_level));
                                obj.insert("thinkingLevelsProbed".to_string(), serde_json::json!(true));
                                updated = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        if updated {
            if let Some(parent) = config_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let serialized = serde_json::to_string_pretty(&root)
                .map_err(|e| ProviderError::Config(format!("Failed to serialize models.json: {e}")))?;
            tokio::fs::write(&config_path, serialized)
                .await
                .map_err(|e| ProviderError::Config(format!("Failed to write {}: {e}", config_path.display())))?;
        }

        Ok(config_path)
    }

    /// Persist utility model identifier to models.json root.
    pub async fn update_utility_model_config(
        utility_model: &str,
    ) -> Result<PathBuf, ProviderError> {
        let config_path = Self::primary_config_path().ok_or_else(|| {
            ProviderError::Config("Cannot locate primary models.json configuration path".to_string())
        })?;

        let raw = if config_path.exists() {
            tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| ProviderError::Config(format!("{}: {e}", config_path.display())))?
        } else {
            "{}".to_string()
        };

        let mut root: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Config(format!("Failed to parse {}: {e}", config_path.display())))?;

        if let Some(obj) = root.as_object_mut() {
            obj.insert("utilityModel".to_string(), serde_json::json!(utility_model));
        }

        if let Some(parent) = config_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let serialized = serde_json::to_string_pretty(&root)
            .map_err(|e| ProviderError::Config(format!("Failed to serialize models.json: {e}")))?;
        tokio::fs::write(&config_path, serialized)
            .await
            .map_err(|e| ProviderError::Config(format!("Failed to write {}: {e}", config_path.display())))?;

        Ok(config_path)
    }
}

impl ProviderFileConfig {
    pub fn api(&self) -> ProviderApi {
        self.api
            .as_deref()
            .and_then(ProviderApi::from_str_loose)
            .unwrap_or_default()
    }
}

impl ModelFileConfig {
    pub fn api_or(&self, fallback: ProviderApi) -> ProviderApi {
        self.api
            .as_deref()
            .and_then(ProviderApi::from_str_loose)
            .unwrap_or(fallback)
    }

    pub fn effective_thinking_levels(&self) -> Option<Vec<String>> {
        if let Some(ref levels) = self.thinking_levels {
            if !levels.is_empty() {
                return Some(levels.clone());
            }
        }
        if let Some(ref map) = self.thinking_level_map {
            let extracted = extract_levels_from_map(map);
            if !extracted.is_empty() {
                return Some(extracted);
            }
        }
        None
    }
}
