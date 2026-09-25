use crate::catalog::ModelSpec;
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClient, LLMClientTrait, LLMStreamChunk};
use tokio_util::sync::CancellationToken;

static SHARED_HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

pub fn shared_http_client() -> reqwest::Client {
    SHARED_HTTP_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .pool_max_idle_per_host(20)
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .tcp_nodelay(true)
                .build()
                .unwrap_or_default()
        })
        .clone()
}

pub fn openai_completions_client(spec: &ModelSpec, timeout_ms: u64) -> Arc<dyn LLMClientTrait> {
    Arc::new(RoutedOpenAiClient::completions(spec, timeout_ms))
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
    inner: Arc<LLMClient>,
    spec: ModelSpec,
}

impl RoutedOpenAiClient {
    pub fn completions(spec: &ModelSpec, timeout_ms: u64) -> Self {
        Self {
            inner: Arc::new(LLMClient::from_client(
                spec.id.clone(),
                normalize_openai_base(&spec.base_url),
                spec.api_key.as_deref(),
                &spec.headers,
                timeout_ms,
                shared_http_client(),
            )),
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

        // Adapt payload using vendor-specific model adapter (DeepSeek, OpenAI reasoning, standard)
        let adapter = crate::adapters::resolve_adapter(&self.spec);
        let payload = adapter.adapt_payload(&self.spec, &options);

        self.inner.stream_chat_payload(payload, cancel_token).await
    }
}
