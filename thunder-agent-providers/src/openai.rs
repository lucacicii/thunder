use crate::catalog::ModelSpec;
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClient, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::ChatMessage;
use tokio_util::sync::CancellationToken;

pub fn openai_completions_client(spec: &ModelSpec, timeout_ms: u64) -> Arc<dyn LLMClientTrait> {
    Arc::new(LLMClient::from_endpoint(
        spec.id.clone(),
        normalize_openai_base(&spec.base_url),
        spec.api_key.as_deref(),
        &spec.headers,
        timeout_ms,
    ))
}

pub fn normalize_openai_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") || trimmed.ends_with("/chat/completions") {
        trimmed.trim_end_matches("/chat/completions").to_string()
    } else if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/v1")
    }
}

pub struct RoutedOpenAiClient {
    inner: Arc<dyn LLMClientTrait>,
    spec: ModelSpec,
}

impl RoutedOpenAiClient {
    pub fn completions(spec: &ModelSpec, timeout_ms: u64) -> Self {
        Self {
            inner: openai_completions_client(spec, timeout_ms),
            spec: spec.clone(),
        }
    }
}

#[async_trait]
impl LLMClientTrait for RoutedOpenAiClient {
    async fn stream_chat(
        &self,
        mut options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        // The wire only understands the bare model id; `provider/model` is
        // Thunder-internal catalog addressing and must never reach the API.
        options.model = Some(self.spec.id.clone());
        if self.spec.supports_developer_role {
            for message in &mut options.messages {
                if let ChatMessage::System { content, name } = message {
                    *message = ChatMessage::System {
                        content: content.clone(),
                        name: name.clone().or_else(|| Some("developer".to_string())),
                    };
                }
            }
        }
        self.inner.stream_chat(options, cancel_token).await
    }
}
