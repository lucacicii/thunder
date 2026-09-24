//! Multi-provider LLM adapter engine for Thunder.
//!
//! Handles vendor-specific protocols, payload transformations, tool mappings,
//! and streaming event adaptations (OpenAI, Anthropic Claude, Google Gemini, Ollama, DeepSeek).

pub mod adapters;
pub mod anthropic;
pub mod api;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod error;
pub mod google;
pub mod openai;
pub mod probe;
pub mod responses;
pub mod source;

use crate::api::ProviderApi;
use crate::catalog::{ModelSpec, ProviderRegistry};
use crate::error::ProviderError;
use std::sync::Arc;
use thunder_agent_loop::stream::client::LLMClientTrait;

pub fn client_for(spec: &ModelSpec, timeout_ms: u64) -> Result<Arc<dyn LLMClientTrait>, ProviderError> {
    if !spec.available {
        return Err(ProviderError::Auth(format!(
            "model `{}` is unavailable (missing API key or base URL)",
            spec.selection_id()
        )));
    }
    match spec.api {
        ProviderApi::OpenAiResponses => {
            Ok(Arc::new(responses::OpenAiResponsesClient::new(spec.clone())))
        }
        ProviderApi::OpenAiCompletions => {
            Ok(Arc::new(openai::RoutedOpenAiClient::completions(spec, timeout_ms)))
        }
        ProviderApi::AnthropicMessages => {
            Ok(Arc::new(adapters::anthropic::AnthropicClient::new(spec.clone())))
        }
        ProviderApi::GoogleGenerateContent => {
            Ok(Arc::new(adapters::google::GoogleClient::new(spec.clone())))
        }
        ProviderApi::Ollama => {
            Ok(Arc::new(adapters::ollama::OllamaClient::new(spec.clone())))
        }
    }
}

pub async fn client_for_selection(
    selection: &str,
    timeout_ms: u64,
) -> Result<(ModelSpec, Arc<dyn LLMClientTrait>), ProviderError> {
    let registry = ProviderRegistry::load_default().await?;
    let spec = registry
        .resolve(selection)
        .cloned()
        .ok_or_else(|| ProviderError::NotFound(selection.to_string()))?;
    let client = client_for(&spec, timeout_ms)?;
    Ok((spec, client))
}

pub async fn has_available_model() -> bool {
    ProviderRegistry::load_default()
        .await
        .map(|registry| registry.list_available().iter().any(|m| m.available))
        .unwrap_or(false)
}

pub mod prelude {
    pub use crate::adapters::{resolve_adapter, ModelAdapter};
    pub use crate::api::{ModelRef, ProviderApi};
    pub use crate::catalog::{
        default_metadata_cache_path, ModelMetadataCache, ModelSpec, ProviderRegistry,
        DEFAULT_SAFE_CONTEXT_WINDOW,
    };
    pub use crate::client_for;
    pub use crate::client_for_selection;
    pub use crate::config::ModelsFile;
    pub use crate::error::ProviderError;
    pub use crate::has_available_model;
    pub use crate::source::ConfigSource;
}
