use crate::api::ProviderApi;
use crate::error::ProviderError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsFile {
    #[serde(default)]
    pub providers: HashMap<String, ProviderFileConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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
}

#[derive(Debug, Clone, Default, Deserialize)]
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
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatConfig {
    pub supports_developer_role: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub max_tokens_field: Option<String>,
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
        let mut paths = vec![PathBuf::from(".pi/models.json")];
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".pi/agent/models.json"));
        }
        paths
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
}
