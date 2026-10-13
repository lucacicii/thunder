//! One-off utility-model completions against an already-resolved model.
//!
//! [`crate::naming`] is the same shape for titles, but it owns a title-specific
//! error type and cleaning rules. This module is the shared raw path: give it a
//! spec, get the model's text back. Callers that resolve their own model (e.g. a
//! tool that summarises a web page with the registry's utility model) use it
//! instead of duplicating the stream pump.

use crate::catalog::ModelSpec;
use crate::client_for;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMStreamChunk};
use thunder_agent_loop::ChatMessage;
use tokio_util::sync::CancellationToken;

/// Stream a single-turn completion from `spec` and return the trimmed text.
///
/// No tools, one user message, no prompt-cache write (`cache_retention: none`)
/// and no session affinity: this is a one-shot request that will never be
/// replayed, so paying the cache-write premium would be pure waste.
///
/// Errors are flattened to `String` because every caller so far falls back to a
/// non-LLM path rather than surfacing a diagnostic type.
pub async fn one_shot_text(
    spec: &ModelSpec,
    user_message: String,
    max_tokens: usize,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Result<String, String> {
    let client = client_for(spec, timeout_ms).map_err(|e| e.to_string())?;

    let options = ChatRequestOptions {
        messages: vec![ChatMessage::user(user_message)],
        tools: vec![],
        model: Some(spec.id.clone()),
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_tokens: Some(max_tokens),
        thinking_level: Some("off".to_string()),
        cache_retention: Some("none".to_string()),
        session_id: None,
    };

    let mut rx = client.stream_chat(options, cancel).await.map_err(|e| {
        format!(
            "utility model `{}` request failed: {e}",
            spec.selection_id()
        )
    })?;

    let mut text = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            Ok(LLMStreamChunk::Token(token)) => text.push_str(&token),
            Ok(LLMStreamChunk::Completed { content, .. }) => {
                if let Some(c) = content {
                    if text.trim().is_empty() {
                        text = c;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                return Err(format!(
                    "utility model `{}` stream error: {e}",
                    spec.selection_id()
                ))
            }
        }
    }

    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(format!("empty completion from {}", spec.selection_id()));
    }
    Ok(text)
}
