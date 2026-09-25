//! `PiAiClient`: the thunder-side `LLMClientTrait` implementation that
//! proxies every model call through the pi-ai bridge sidecar.

use crate::model::BridgeModel;
use crate::process::{default_bridge_dir, PiAiBridge};
use async_trait::async_trait;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::stream::client::{
    ChatRequestOptions, LLMClientTrait, LLMStreamChunk,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Clone)]
pub struct PiAiClient {
    bridge: Option<Arc<PiAiBridge>>,
    model: BridgeModel,
    request_timeout_ms: u64,
}

impl PiAiClient {
    pub fn new(bridge: Arc<PiAiBridge>, model: BridgeModel, request_timeout_ms: u64) -> Self {
        Self {
            bridge: Some(bridge),
            model,
            request_timeout_ms,
        }
    }

    /// Construct a client that resolves `global_bridge()` lazily on the first
    /// `stream_chat` invocation. This allows synchronous creation of `LLMClientTrait`.
    pub fn new_lazy(model: BridgeModel, request_timeout_ms: u64) -> Self {
        Self {
            bridge: None,
            model,
            request_timeout_ms,
        }
    }

    /// Build a client against the process-global bridge instance
    /// (bridge dir `~/.thunder/bridge`, auto-installed on first boot).
    pub async fn global(model: BridgeModel, request_timeout_ms: u64) -> Result<Self, String> {
        Ok(Self::new(global_bridge().await?, model, request_timeout_ms))
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamRequest<'a> {
    cmd: &'static str,
    id: String,
    model: &'a BridgeModel,
    messages: &'a [thunder_agent_loop::types::message::ChatMessage],
    tools: &'a [thunder_agent_loop::types::tool::ToolDefinition],
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
}

#[async_trait]
impl LLMClientTrait for PiAiClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let bridge = match &self.bridge {
            Some(b) => Arc::clone(b),
            None => global_bridge().await.map_err(|e| format!("pi-ai bridge init failed: {e}"))?,
        };
        let id = bridge.next_request_id();

        let (tx, rx) = mpsc::channel::<Result<LLMStreamChunk, String>>(64);
        let last_activity = bridge.register_stream(id.clone(), tx.clone()).await?;

        let request = StreamRequest {
            cmd: "stream",
            id: id.clone(),
            model: &self.model,
            messages: &options.messages,
            tools: &options.tools,
            thinking_level: options.thinking_level.clone(),
            temperature: options.temperature,
            top_p: options.top_p,
            max_tokens: options.max_tokens,
        };
        let payload = serde_json::to_string(&request)
            .map_err(|e| format!("failed to serialize bridge request: {e}"))?;

        if let Err(err) = bridge.send_request(payload).await {
            bridge.unregister(&id).await;
            return Err(err);
        }

        // Watchdog: cancellation passthrough + chunk-idle timeout.
        let bridge_ref = Arc::clone(&bridge);
        let watch_id = id.clone();
        let idle_limit_ms = self.request_timeout_ms.max(60_000);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        let _ = bridge_ref.send_line(
                            serde_json::json!({ "cmd": "cancel", "id": watch_id }).to_string(),
                        ).await;
                        let _ = tx.send(Err("Request cancelled by user".to_string())).await;
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if tx.is_closed() {
                            break;
                        }
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        if now.saturating_sub(last_activity.load(Ordering::Relaxed)) > idle_limit_ms {
                            warn!(id = %watch_id, idle_ms = idle_limit_ms, "Bridge stream idle timeout, cancelling");
                            let _ = bridge_ref.send_line(
                                serde_json::json!({ "cmd": "cancel", "id": watch_id }).to_string(),
                            ).await;
                            let _ = tx.send(Err("Bridge stream idle timeout".to_string())).await;
                            break;
                        }
                    }
                }
            }
            bridge_ref.unregister(&watch_id).await;
        });

        Ok(rx)
    }
}

/// Process-global bridge instance. First call bootstraps `~/.thunder/bridge`
/// (runner files are copied there; pi-ai is resolved from
/// `$THUNDER_PI_AI_PATH`, that dir's node_modules, a global pi installation,
/// or an automatic `npm install` on first boot).
///
/// Launch failures are NOT cached: a missing node/npm on first call can be
/// fixed and the next call retries transparently.
pub async fn global_bridge() -> Result<Arc<PiAiBridge>, String> {
    static GLOBAL: tokio::sync::Mutex<Option<Arc<PiAiBridge>>> = tokio::sync::Mutex::const_new(None);
    let mut guard = GLOBAL.lock().await;
    if let Some(bridge) = guard.as_ref() {
        return Ok(Arc::clone(bridge));
    }
    let pi_ai_path = std::env::var_os("THUNDER_PI_AI_PATH").map(std::path::PathBuf::from);
    let bridge = PiAiBridge::launch(default_bridge_dir(), pi_ai_path).await?;
    *guard = Some(Arc::clone(&bridge));
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_request_serializes_thunder_shapes() {
        let mut model = BridgeModel::new("p", "m", crate::model::API_OPENAI_COMPLETIONS);
        model.base_url = "https://x".into();
        let req = StreamRequest {
            cmd: "stream",
            id: "req-1".into(),
            model: &model,
            messages: &[thunder_agent_loop::types::message::ChatMessage::user("hi")],
            tools: &[],
            thinking_level: Some("high".into()),
            temperature: Some(0.2),
            top_p: None,
            max_tokens: Some(128),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["cmd"], "stream");
        assert_eq!(v["thinkingLevel"], "high");
        assert_eq!(v["maxTokens"], 128);
        // None fields are skipped entirely (no nulls on the wire)
        assert!(v.get("topP").is_none());
        assert_eq!(v["messages"][0]["role"], "user");
    }
}
