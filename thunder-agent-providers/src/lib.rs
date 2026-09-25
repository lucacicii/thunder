//! Multi-provider LLM adapter engine for Thunder.
//!
//! Exposes pi-ai compatible models.json / auth.json configuration parsing and
//! proxies streaming LLM execution through `@earendil-works/pi-ai` via `thunder-pi-bridge`.

pub mod api;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod error;
pub mod probe;
pub mod source;

pub mod openai {
    pub use crate::api::normalize_openai_base;
}

use crate::catalog::{ModelSpec, ProviderRegistry};
use crate::error::ProviderError;
use std::sync::Arc;
use thunder_agent_loop::stream::client::LLMClientTrait;
use thunder_pi_bridge::PiAiClient;

pub fn client_for(spec: &ModelSpec, timeout_ms: u64) -> Result<Arc<dyn LLMClientTrait>, ProviderError> {
    if !spec.available {
        return Err(ProviderError::Auth(format!(
            "model `{}` is unavailable (missing API key or base URL)",
            spec.selection_id()
        )));
    }
    let bridge_model = spec.to_bridge_model();
    Ok(Arc::new(PiAiClient::new_lazy(bridge_model, timeout_ms)))
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
    pub use crate::api::{normalize_openai_base, ModelRef, ProviderApi};
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
