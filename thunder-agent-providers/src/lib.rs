//! Multi-provider LLM adapter engine for Thunder.
//!
//! Exposes pi-ai compatible models.json / auth.json configuration parsing and
//! streams from the resolved model over HTTP, in process ([`client::RpiAiClient`]).

pub mod api;
pub mod auth;
pub mod catalog;
pub mod client;
pub mod config;
pub mod convert;
pub mod error;
pub mod model;
pub mod naming;
pub mod probe;
pub mod source;

pub mod openai {
    pub use crate::api::normalize_openai_base;
}

// The two names a consumer outside this crate needs: the model description it
// sends over the wire, and the client that streams it.
pub use crate::client::RpiAiClient;
pub use crate::model::{ModelCost, ModelDescriptor, ModelPromptCache};

use crate::catalog::{ModelSpec, ProviderRegistry};
use crate::error::ProviderError;
use std::sync::Arc;
use thunder_agent_loop::stream::client::LLMClientTrait;

pub fn client_for(
    spec: &ModelSpec,
    timeout_ms: u64,
) -> Result<Arc<dyn LLMClientTrait>, ProviderError> {
    if !spec.available {
        return Err(ProviderError::Auth(format!(
            "model `{}` is unavailable (missing API key or base URL)",
            spec.selection_id()
        )));
    }
    let descriptor = spec.to_descriptor();
    // A dialect this build cannot stream is reported as such, naming the model:
    // the catalog's `available` flag only speaks about credentials, so this is
    // the first place a user learns the difference.
    if !convert::supports_api(&descriptor.api) {
        return Err(ProviderError::Config(format!(
            "model `{}`: {}",
            spec.selection_id(),
            convert::unsupported_api_message(&descriptor.api)
        )));
    }
    let (client, notes) =
        client::RpiAiClient::from_descriptor(&descriptor, timeout_ms).map_err(|err| {
            ProviderError::Transport(format!("model `{}`: {err}", spec.selection_id()))
        })?;
    if !notes.is_empty() {
        tracing::warn!(
            model = %spec.selection_id(),
            notes = ?notes.describe(),
            "model descriptor lost fields the stream client cannot carry"
        );
    }
    Ok(Arc::new(client))
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
    pub use crate::client::RpiAiClient;
    pub use crate::client_for;
    pub use crate::client_for_selection;
    pub use crate::config::ModelsFile;
    pub use crate::convert::{supports_api, ConversionNotes, SUPPORTED_APIS};
    pub use crate::error::ProviderError;
    pub use crate::has_available_model;
    pub use crate::model::{ModelCost, ModelDescriptor, ModelPromptCache};
    pub use crate::naming::{clamp_title, clean_generated_title, generate_title, TitleGenError};
    pub use crate::source::ConfigSource;
}
