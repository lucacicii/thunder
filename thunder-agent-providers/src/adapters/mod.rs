pub mod anthropic;
pub mod deepseek;
pub mod google;
pub mod ollama;
pub mod openai;

use crate::api::ProviderApi;
use crate::catalog::ModelSpec;
use serde_json::Value;
use thunder_agent_loop::stream::client::ChatRequestOptions;

/// Trait defining vendor and model-specific protocol adaptations.
pub trait ModelAdapter: Send + Sync {
    /// Identifier of the adapter (e.g. "openai-standard", "openai-reasoning", "deepseek-v4", "anthropic", "google")
    fn name(&self) -> &'static str;

    /// Normalize and adapt generic ChatRequestOptions and payload for the specific target model
    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value;
}

/// Helper to check if a model ID represents an OpenAI reasoning model (o1, o3 series)
pub fn is_openai_reasoning_model(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    lower.starts_with("o1") || lower.starts_with("o3") || lower.contains("/o1") || lower.contains("/o3")
}

/// Helper to check if a model belongs to the DeepSeek family (V4.1-Flash, V4-Pro, V3, R1)
pub fn is_deepseek_family(provider: &str, model_id: &str) -> bool {
    let prov = provider.to_lowercase();
    let id = model_id.to_lowercase();
    prov.contains("deepseek") || id.contains("deepseek")
}

/// Select the optimal adapter based on ProviderApi and model specifications
pub fn resolve_adapter(spec: &ModelSpec) -> Box<dyn ModelAdapter> {
    match spec.api {
        ProviderApi::AnthropicMessages => Box::new(anthropic::AnthropicAdapter),
        ProviderApi::GoogleGenerateContent => Box::new(google::GoogleAdapter),
        ProviderApi::Ollama => Box::new(ollama::OllamaAdapter),
        ProviderApi::OpenAiResponses => Box::new(openai::OpenAiStandardAdapter),
        ProviderApi::OpenAiCompletions => {
            if is_deepseek_family(&spec.provider, &spec.id) {
                Box::new(deepseek::DeepSeekAdapter)
            } else if is_openai_reasoning_model(&spec.id) || spec.supports_reasoning_effort {
                Box::new(openai::OpenAiReasoningAdapter)
            } else {
                Box::new(openai::OpenAiStandardAdapter)
            }
        }
    }
}
