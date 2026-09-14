use crate::api::{ModelRef, ProviderApi};
use crate::auth::{resolve_provider_key, AuthFile};
use crate::config::{CompatConfig, ModelsFile};
use crate::error::ProviderError;
use std::collections::HashMap;

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
}

impl ModelSpec {
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
    pub models: Vec<ModelSpec>,
}

impl ProviderRegistry {
    pub fn from_parts(file: ModelsFile, auth: &AuthFile) -> Result<Self, ProviderError> {
        let mut models = Vec::new();
        for (provider_id, provider) in file.providers {
            let provider_api = provider.api();
            let provider_base = provider.base_url.clone().unwrap_or_default();
            let provider_key = resolve_provider_key(&provider_id, provider.api_key.as_deref(), auth)?;
            let provider_compat = provider.compat.clone().unwrap_or_default();

            if provider.models.is_empty() {
                if let Some(builtin) = builtin_models(&provider_id, provider_api, &provider_base, provider_key.clone(), &provider.headers, &provider_compat) {
                    models.extend(builtin);
                }
                continue;
            }

            for model in provider.models {
                let api = model.api_or(provider_api);
                let compat = merge_compat(&provider_compat, model.compat.as_ref());
                let base_url = model.base_url.clone().unwrap_or_else(|| provider_base.clone());
                let has_key = provider_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);
                models.push(ModelSpec {
                    provider: provider_id.clone(),
                    id: model.id.clone(),
                    name: model.name.clone().unwrap_or_else(|| model.id.clone()),
                    api,
                    base_url: base_url.clone(),
                    api_key: provider_key.clone(),
                    headers: provider.headers.clone(),
                    reasoning: model.reasoning,
                    context_window: model.context_window.unwrap_or(128_000),
                    max_tokens: model.max_tokens.unwrap_or(16_384),
                    available: has_key && !base_url.trim().is_empty(),
                    supports_developer_role: compat.supports_developer_role.unwrap_or(false),
                    supports_reasoning_effort: compat.supports_reasoning_effort.unwrap_or(false),
                    max_tokens_field: compat
                        .max_tokens_field
                        .unwrap_or_else(|| "max_tokens".to_string()),
                });
            }
        }

        if models.is_empty() {
            models.extend(fallback_openai_catalog(auth));
        }

        Ok(Self { models })
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
            registry.models.extend(fallback_openai_catalog(&auth));
        }
        Ok(registry)
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
                    self.models.iter().find(|m| m.provider == provider && m.id == id)
                })
            })
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
) -> Option<Vec<ModelSpec>> {
    let defs: &[(&str, &str, bool)] = match provider {
        "openai" => &[
            ("gpt-4o", "GPT-4o", false),
            ("gpt-4o-mini", "GPT-4o Mini", false),
        ],
        "deepseek" => &[
            ("deepseek-chat", "DeepSeek V3", false),
            ("deepseek-reasoner", "DeepSeek R1", true),
        ],
        _ => return None,
    };
    let resolved_base = if base_url.trim().is_empty() && provider == "openai" {
        "https://api.openai.com/v1"
    } else {
        base_url
    };
    let has_key = api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);

    Some(
        defs.iter()
            .map(|(id, name, reasoning)| ModelSpec {
                provider: provider.to_string(),
                id: (*id).to_string(),
                name: (*name).to_string(),
                api,
                base_url: resolved_base.to_string(),
                api_key: api_key.clone(),
                headers: headers.clone(),
                reasoning: *reasoning,
                context_window: 128_000,
                max_tokens: 16_384,
                available: has_key && !resolved_base.trim().is_empty(),
                supports_developer_role: compat.supports_developer_role.unwrap_or(false),
                supports_reasoning_effort: compat.supports_reasoning_effort.unwrap_or(false),
                max_tokens_field: compat
                    .max_tokens_field
                    .clone()
                    .unwrap_or_else(|| "max_tokens".to_string()),
            })
            .collect(),
    )
}

fn fallback_openai_catalog(auth: &AuthFile) -> Vec<ModelSpec> {
    let key = resolve_provider_key("openai", None, auth).ok().flatten();
    let has_key = key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);
    ["gpt-4o", "gpt-4o-mini"]
        .into_iter()
        .map(|id| ModelSpec {
            provider: "openai".to_string(),
            id: id.to_string(),
            name: id.to_string(),
            api: ProviderApi::OpenAiCompletions,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: key.clone(),
            headers: HashMap::new(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 16_384,
            available: has_key,
            supports_developer_role: false,
            supports_reasoning_effort: false,
            max_tokens_field: "max_tokens".to_string(),
        })
        .collect()
}
