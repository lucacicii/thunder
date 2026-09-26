use crate::api::{ModelRef, ProviderApi};
use crate::auth::{resolve_provider_key, AuthFile};
use crate::config::{CompatConfig, ModelsFile};
use crate::error::ProviderError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_SAFE_CONTEXT_WINDOW: usize = 128_000;

pub fn resolve_thinking_levels(
    explicit_levels: Option<&[String]>,
    explicit_default: Option<&str>,
    reasoning: bool,
    supports_reasoning_effort: bool,
    model_id: &str,
) -> (Vec<String>, String) {
    let default_reasoning_levels = vec![
        "off".to_string(),
        "low".to_string(),
        "medium".to_string(),
        "high".to_string(),
    ];

    let is_known_reasoning = reasoning || supports_reasoning_effort || {
        let lower = model_id.to_lowercase();
        lower.contains("reasoner")
            || lower.contains("r1")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.contains("/o1")
            || lower.contains("/o3")
            || lower.contains("thinking")
            || lower.contains("sonnet-3-7")
            || lower.contains("claude-3-7")
    };

    let levels = if let Some(explicit) = explicit_levels {
        if explicit.is_empty() {
            vec!["off".to_string()]
        } else {
            crate::config::sort_thinking_levels(explicit)
        }
    } else if is_known_reasoning {
        default_reasoning_levels
    } else {
        vec!["off".to_string()]
    };

    let default_level = if let Some(explicit_def) = explicit_default {
        explicit_def.to_string()
    } else if is_known_reasoning {
        let lower = model_id.to_lowercase();
        if lower.contains("reasoner") || lower.contains("r1") {
            "high".to_string()
        } else if levels.contains(&"medium".to_string()) {
            "medium".to_string()
        } else if levels.contains(&"high".to_string()) {
            "high".to_string()
        } else {
            levels.first().cloned().unwrap_or_else(|| "off".to_string())
        }
    } else {
        "off".to_string()
    };

    (levels, default_level)
}

/// Resolves standard location for runtime learned model specifications cache.
pub fn default_metadata_cache_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".thunder")
            .join("models_metadata.json")
    } else if let Ok(userprofile) = std::env::var("USERPROFILE") {
        PathBuf::from(userprofile)
            .join(".thunder")
            .join("models_metadata.json")
    } else {
        std::env::temp_dir().join("thunder_models_metadata.json")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelMetadataCache {
    /// Mapping of model selection_id or id -> detected context window limit
    #[serde(default)]
    pub context_windows: HashMap<String, usize>,
}

impl ModelMetadataCache {
    pub fn load_default() -> Self {
        Self::load_from_file(&default_metadata_cache_path())
    }

    pub fn load_from_file(path: &Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }

    /// Resolve context window deterministically:
    /// 1. Explicit user configuration from models.json (highest priority)
    /// 2. Locally learned and cached runtime probe result
    /// 3. Conservative safe default (128k)
    pub fn resolve_context_window(
        &self,
        selection_id: &str,
        model_id: &str,
        configured: Option<usize>,
    ) -> usize {
        if let Some(c) = configured {
            return c;
        }
        if let Some(&cached) = self.context_windows.get(selection_id) {
            return cached;
        }
        if let Some(&cached) = self.context_windows.get(model_id) {
            return cached;
        }
        DEFAULT_SAFE_CONTEXT_WINDOW
    }
}

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: ProviderApi,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: HashMap<String, String>,
    pub reasoning: bool,
    pub context_window: usize,
    pub max_tokens: usize,
    pub available: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub max_tokens_field: String,
    pub thinking_levels: Vec<String>,
    pub default_thinking_level: String,
    pub thinking_levels_probed: bool,
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    pub compat: Option<serde_json::Value>,
}

impl ModelSpec {
    pub fn to_bridge_model(&self) -> thunder_pi_bridge::BridgeModel {
        let mut m =
            thunder_pi_bridge::BridgeModel::new(&self.provider, &self.id, self.api.as_pi_api_str());
        m.name = self.name.clone();
        m.base_url = self.base_url.clone();
        m.api_key = self.api_key.clone();
        m.headers = self.headers.clone();
        m.reasoning = self.reasoning;
        m.context_window = self.context_window;
        m.max_tokens = self.max_tokens;
        m.thinking_level_map = self.thinking_level_map.clone();
        m.compat = self.compat.clone();
        m
    }

    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(&self.provider, &self.id)
    }

    pub fn selection_id(&self) -> String {
        self.model_ref().selection_id()
    }

    pub fn picker_title(&self) -> String {
        if self.name == self.id {
            self.id.clone()
        } else {
            format!("{} ({})", self.name, self.id)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    pub utility_model: Option<String>,
    pub models: Vec<ModelSpec>,
}

impl ProviderRegistry {
    pub fn from_parts(file: ModelsFile, auth: &AuthFile) -> Result<Self, ProviderError> {
        let cache = ModelMetadataCache::load_default();
        let mut models = Vec::new();
        for (provider_id, provider) in file.providers {
            let provider_api = provider.api();
            let provider_base = provider.base_url.clone().unwrap_or_default();
            let provider_key =
                resolve_provider_key(&provider_id, provider.api_key.as_deref(), auth)?;
            let provider_compat = provider.compat.clone().unwrap_or_default();

            if provider.models.is_empty() {
                if let Some(builtin) = builtin_models(
                    &provider_id,
                    provider_api,
                    &provider_base,
                    provider_key.clone(),
                    &provider.headers,
                    &provider_compat,
                    &cache,
                ) {
                    models.extend(builtin);
                }
                continue;
            }

            for model in provider.models {
                let api = model.api_or(provider_api);
                let compat = merge_compat(&provider_compat, model.compat.as_ref());
                let base_url = model
                    .base_url
                    .clone()
                    .unwrap_or_else(|| provider_base.clone());
                let has_key = provider_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false);
                let selection_id = format!("{}/{}", provider_id, model.id);
                let context_window =
                    cache.resolve_context_window(&selection_id, &model.id, model.context_window);
                let effective_levels = model
                    .effective_thinking_levels()
                    .or_else(|| provider.thinking_levels.clone())
                    .or_else(|| {
                        provider
                            .thinking_level_map
                            .as_ref()
                            .map(crate::config::extract_levels_from_map)
                    });
                let (thinking_levels, default_thinking_level) = resolve_thinking_levels(
                    effective_levels.as_deref(),
                    model
                        .default_thinking_level
                        .as_deref()
                        .or(provider.default_thinking_level.as_deref()),
                    model.reasoning,
                    compat.supports_reasoning_effort.unwrap_or(false),
                    &model.id,
                );

                let thinking_level_map = model
                    .thinking_level_map
                    .clone()
                    .or_else(|| provider.thinking_level_map.clone());
                let compat_val = serde_json::to_value(&compat).ok();

                models.push(ModelSpec {
                    provider: provider_id.clone(),
                    id: model.id.clone(),
                    name: model.name.clone().unwrap_or_else(|| model.id.clone()),
                    api,
                    base_url: base_url.clone(),
                    api_key: provider_key.clone(),
                    headers: provider.headers.clone(),
                    reasoning: model.reasoning,
                    context_window,
                    max_tokens: model.max_tokens.unwrap_or(16_384),
                    available: has_key && !base_url.trim().is_empty(),
                    supports_developer_role: compat.supports_developer_role.unwrap_or(false),
                    supports_reasoning_effort: compat.supports_reasoning_effort.unwrap_or(false),
                    max_tokens_field: compat
                        .max_tokens_field
                        .unwrap_or_else(|| "max_tokens".to_string()),
                    thinking_levels,
                    default_thinking_level,
                    thinking_levels_probed: model.thinking_levels_probed.unwrap_or(false),
                    thinking_level_map,
                    compat: compat_val,
                });
            }
        }

        if models.is_empty() {
            models.extend(fallback_openai_catalog(auth, &cache));
        }

        Ok(Self {
            utility_model: file.utility_model,
            models,
        })
    }

    pub async fn load_default() -> Result<Self, ProviderError> {
        Self::load_from_sources(&crate::source::ConfigSource::default_chain(None)).await
    }

    pub async fn load_from_sources(
        sources: &[crate::source::ConfigSource],
    ) -> Result<Self, ProviderError> {
        let (file, auth) = crate::source::load_merged(sources).await?;
        let mut registry = Self::from_parts(file, &auth)?;
        if registry.models.is_empty() {
            let cache = ModelMetadataCache::load_default();
            registry
                .models
                .extend(fallback_openai_catalog(&auth, &cache));
        }

        // Active probing for unprobed reasoning models
        registry.probe_unprobed_models().await;

        Ok(registry)
    }

    /// Actively probe unprobed reasoning models.
    /// If probe succeeds, updates in-memory spec, writes back to models.json, and sets thinkingLevelsProbed flag.
    /// If already probed (thinking_levels_probed is true), skips probing completely.
    pub async fn probe_unprobed_models(&mut self) {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        for m in &mut self.models {
            if m.thinking_levels_probed {
                continue;
            }

            let is_reasoning_candidate = m.reasoning || m.supports_reasoning_effort || {
                let lower = m.id.to_lowercase();
                lower.contains("reasoner")
                    || lower.contains("r1")
                    || lower.starts_with("o1")
                    || lower.starts_with("o3")
                    || lower.contains("thinking")
                    || lower.contains("sonnet-3-7")
                    || lower.contains("claude-3-7")
            };

            if !is_reasoning_candidate || !m.available {
                continue;
            }

            tracing::info!(
                "Actively probing thinking capabilities for model: {}",
                m.selection_id()
            );
            if let Some(probe_result) = crate::probe::probe_model_thinking_levels(&client, m).await
            {
                tracing::info!(
                    "Successfully probed thinking levels for {}: {:?}, default: {}",
                    m.selection_id(),
                    probe_result.thinking_levels,
                    probe_result.default_thinking_level
                );
                m.thinking_levels = probe_result.thinking_levels.clone();
                m.default_thinking_level = probe_result.default_thinking_level.clone();
                m.thinking_levels_probed = true;

                let _ = crate::config::ModelsFile::update_model_thinking_probed_config(
                    &m.provider,
                    &m.id,
                    &probe_result.thinking_levels,
                    &probe_result.default_thinking_level,
                )
                .await;
            } else {
                tracing::debug!(
                    "Active probe for {} did not return thinking levels; keeping existing configuration",
                    m.selection_id()
                );
            }
        }
    }

    /// Resolve configured or heuristic utility model (for background naming, summaries, tools).
    pub fn resolve_utility_model(&self) -> Option<&ModelSpec> {
        if let Some(ref u) = self.utility_model {
            if let Some(spec) = self.resolve(u) {
                return Some(spec);
            }
        }

        // Fallback: look for fast/cheap available model (flash, mini, haiku, lite).
        // Prefer non-reasoning models: reasoning models burn the small token budget
        // on thinking and often return empty content for background tasks like naming.
        let fast_candidates = ["flash", "mini", "haiku", "lite", "turbo", "small"];
        for candidate in fast_candidates {
            if let Some(spec) = self.models.iter().find(|m| {
                m.available
                    && !m.reasoning
                    && (m.id.to_lowercase().contains(candidate)
                        || m.name.to_lowercase().contains(candidate))
            }) {
                return Some(spec);
            }
        }

        // Second pass: allow reasoning models as a last resort for fast candidates
        for candidate in fast_candidates {
            if let Some(spec) = self.models.iter().find(|m| {
                m.available
                    && (m.id.to_lowercase().contains(candidate)
                        || m.name.to_lowercase().contains(candidate))
            }) {
                return Some(spec);
            }
        }

        // Fallback: first available non-reasoning model, then any available model
        self.models
            .iter()
            .find(|m| m.available && !m.reasoning)
            .or_else(|| self.models.iter().find(|m| m.available))
            .or_else(|| self.models.first())
    }

    pub fn resolve_ref(&self, model_ref: &crate::api::ModelRef) -> Option<&ModelSpec> {
        self.resolve(&model_ref.selection_id())
    }

    pub fn list_available(&self) -> Vec<&ModelSpec> {
        let available: Vec<_> = self.models.iter().filter(|m| m.available).collect();
        if available.is_empty() {
            self.models.iter().collect()
        } else {
            available
        }
    }

    pub fn resolve(&self, selection: &str) -> Option<&ModelSpec> {
        let trimmed = selection.trim();
        self.models
            .iter()
            .find(|m| m.selection_id() == trimmed)
            .or_else(|| self.models.iter().find(|m| m.id == trimmed))
            .or_else(|| {
                trimmed.split_once('/').and_then(|(provider, id)| {
                    self.models
                        .iter()
                        .find(|m| m.provider == provider && m.id == id)
                })
            })
    }

    /// Dynamically update a model's context window after real error detection or runtime confirmation,
    /// and asynchronously persist it to the local cache file.
    pub fn update_model_context_window(&mut self, selection_id: &str, real_limit: usize) -> bool {
        let trimmed = selection_id.trim();
        let mut updated = false;
        let mut resolved_selection = trimmed.to_string();

        for m in &mut self.models {
            if m.selection_id() == trimmed
                || m.id == trimmed
                || format!("{}/{}", m.provider, m.id) == trimmed
            {
                m.context_window = real_limit;
                resolved_selection = m.selection_id();
                updated = true;
            }
        }

        if updated {
            let cache_path = default_metadata_cache_path();
            let mut cache = ModelMetadataCache::load_from_file(&cache_path);
            cache.context_windows.insert(resolved_selection, real_limit);
            let _ = cache.save_to_file(&cache_path);
        }

        updated
    }
}

fn merge_compat(provider: &CompatConfig, model: Option<&CompatConfig>) -> CompatConfig {
    let mut out = provider.clone();
    if let Some(model) = model {
        if model.supports_developer_role.is_some() {
            out.supports_developer_role = model.supports_developer_role;
        }
        if model.supports_reasoning_effort.is_some() {
            out.supports_reasoning_effort = model.supports_reasoning_effort;
        }
        if model.max_tokens_field.is_some() {
            out.max_tokens_field = model.max_tokens_field.clone();
        }
    }
    out
}

fn builtin_models(
    provider: &str,
    api: ProviderApi,
    base_url: &str,
    api_key: Option<String>,
    headers: &HashMap<String, String>,
    compat: &CompatConfig,
    cache: &ModelMetadataCache,
) -> Option<Vec<ModelSpec>> {
    let defs: &[(&str, &str, bool)] = match provider {
        "openai" => &[
            ("gpt-4o", "GPT-4o", false),
            ("gpt-4o-mini", "GPT-4o Mini", false),
        ],
        "deepseek" => &[
            ("deepseek-flash", "DeepSeek V4.1 Flash", true),
            ("deepseek-v4-pro", "DeepSeek V4 Pro", true),
            ("deepseek-chat", "DeepSeek V3", false),
            ("deepseek-reasoner", "DeepSeek R1", true),
        ],
        _ => return None,
    };
    let resolved_base = if base_url.trim().is_empty() {
        if provider == "openai" {
            "https://api.openai.com/v1"
        } else if provider == "deepseek" {
            "https://api.deepseek.com"
        } else {
            base_url
        }
    } else {
        base_url
    };
    let has_key = api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);

    Some(
        defs.iter()
            .map(|(id, name, reasoning)| {
                let sel_id = format!("{}/{}", provider, id);
                let context_window = cache.resolve_context_window(&sel_id, id, None);
                let (thinking_levels, default_thinking_level) = resolve_thinking_levels(
                    None,
                    None,
                    *reasoning,
                    compat.supports_reasoning_effort.unwrap_or(false),
                    id,
                );
                ModelSpec {
                    provider: provider.to_string(),
                    id: (*id).to_string(),
                    name: (*name).to_string(),
                    api,
                    base_url: resolved_base.to_string(),
                    api_key: api_key.clone(),
                    headers: headers.clone(),
                    reasoning: *reasoning,
                    context_window,
                    max_tokens: 16_384,
                    available: has_key && !resolved_base.trim().is_empty(),
                    supports_developer_role: compat.supports_developer_role.unwrap_or(false),
                    supports_reasoning_effort: compat.supports_reasoning_effort.unwrap_or(false),
                    max_tokens_field: compat
                        .max_tokens_field
                        .clone()
                        .unwrap_or_else(|| "max_tokens".to_string()),
                    thinking_levels,
                    default_thinking_level,
                    thinking_levels_probed: false,
                    thinking_level_map: None,
                    compat: serde_json::to_value(compat).ok(),
                }
            })
            .collect(),
    )
}

fn fallback_openai_catalog(auth: &AuthFile, cache: &ModelMetadataCache) -> Vec<ModelSpec> {
    let key = resolve_provider_key("openai", None, auth).ok().flatten();
    let has_key = key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);
    ["gpt-4o", "gpt-4o-mini"]
        .into_iter()
        .map(|id| {
            let sel_id = format!("openai/{}", id);
            let context_window = cache.resolve_context_window(&sel_id, id, None);
            let (thinking_levels, default_thinking_level) =
                resolve_thinking_levels(None, None, false, false, id);
            ModelSpec {
                provider: "openai".to_string(),
                id: id.to_string(),
                name: id.to_string(),
                api: ProviderApi::OpenAiCompletions,
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: key.clone(),
                headers: HashMap::new(),
                reasoning: false,
                context_window,
                max_tokens: 16_384,
                available: has_key,
                supports_developer_role: false,
                supports_reasoning_effort: false,
                max_tokens_field: "max_tokens".to_string(),
                thinking_levels,
                default_thinking_level,
                thinking_levels_probed: false,
                thinking_level_map: None,
                compat: None,
            }
        })
        .collect()
}
