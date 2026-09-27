//! Background utility-model naming: conversation title generation.
//!
//! Pure LLM-side helper shared by every host (daemon, TUI): resolves the
//! utility model, streams a tiny one-off completion, and normalizes the
//! result. Hosts own persistence and the manual-title policy — that is
//! product decisioning, not transport.

use crate::catalog::ProviderRegistry;
use crate::client_for;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMStreamChunk};
use thunder_agent_loop::ChatMessage;

/// Structured error for title generation, surfaced to the host for diagnosis.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TitleGenError {
    /// Machine-readable kind: no_utility_model | client_error | api_error |
    /// empty_title
    pub kind: String,
    /// Human-readable detail (include model id / source error where available)
    pub detail: String,
}

impl TitleGenError {
    pub fn new(kind: &str, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for TitleGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.kind, self.detail)
    }
}

/// Strip quotes/prefixes from a raw model-generated title.
pub fn clean_generated_title(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '《' || c == '》' || c == '【' || c == '】' || c == '`'
        })
        .trim_start_matches("Title:")
        .trim_start_matches("标题:")
        .trim_start_matches("标题：")
        .trim()
        .to_string()
}

/// Truncate to a display-safe length (char-boundary safe).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

/// Truncate to a display-safe title length (char-boundary safe).
pub fn clamp_title(cleaned: String) -> String {
    if cleaned.chars().count() > 40 {
        cleaned.chars().take(40).collect()
    } else {
        cleaned
    }
}

/// Stream a title for the given exchange from the registry's utility model.
///
/// Never touches conversation storage: callers pass the excerpts and own what
/// happens with the returned string.
pub async fn generate_title(
    registry: &ProviderRegistry,
    user_prompt: &str,
    assistant_text: Option<&str>,
) -> Result<String, TitleGenError> {
    // Resolve the utility model used for background naming
    let utility_spec = match registry.resolve_utility_model() {
        Some(s) if s.available => s.clone(),
        Some(s) => {
            return Err(TitleGenError::new(
                "no_utility_model",
                format!(
                    "utility model `{}` is not available (missing API key or base URL)",
                    s.selection_id()
                ),
            ));
        }
        None => {
            return Err(TitleGenError::new(
                "no_utility_model",
                "no utility model configured or heuristically resolvable; set `utilityModel` in models.json",
            ));
        }
    };

    let client = client_for(&utility_spec, 60_000).map_err(|e| {
        TitleGenError::new(
            "client_error",
            format!(
                "failed to create client for `{}`: {e}",
                utility_spec.selection_id()
            ),
        )
    })?;

    let user_excerpt = truncate_chars(user_prompt, 300);
    let assistant_excerpt = assistant_text
        .map(|t| truncate_chars(t, 300))
        .unwrap_or_default();

    let naming_instruction = "You are a concise title generator. Generate a concise, descriptive conversation title (between 4 and 10 Chinese characters or 2 to 6 English words, no punctuation, no quotes, no explanations, no prefix like 'Title:') summarizing the exchange.\n\nUser: ".to_string()
        + &user_excerpt
        + "\nAssistant: "
        + &assistant_excerpt;

    let options = ChatRequestOptions {
        messages: vec![ChatMessage::user(naming_instruction)],
        tools: vec![],
        model: Some(utility_spec.id.clone()),
        temperature: Some(0.3),
        top_p: Some(0.9),
        // Generous budget: reasoning models may spend tokens thinking before the answer
        max_tokens: Some(100),
        thinking_level: Some("off".to_string()),
        // One-off utility call: never pay the prompt-cache write premium.
        cache_retention: Some("none".to_string()),
    };

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let mut rx = client
        .stream_chat(options, cancel_token)
        .await
        .map_err(|e| {
            TitleGenError::new(
                "api_error",
                format!(
                    "utility model `{}` request failed: {e}",
                    utility_spec.selection_id()
                ),
            )
        })?;

    let mut generated_title = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            Ok(LLMStreamChunk::Token(token)) => {
                generated_title.push_str(&token);
            }
            Ok(LLMStreamChunk::Completed { content, .. }) => {
                if let Some(c) = content {
                    if generated_title.trim().is_empty() {
                        generated_title = c;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                return Err(TitleGenError::new(
                    "api_error",
                    format!(
                        "utility model `{}` stream error: {e}",
                        utility_spec.selection_id()
                    ),
                ));
            }
        }
    }

    let cleaned = clean_generated_title(&generated_title);
    if cleaned.is_empty() {
        return Err(TitleGenError::new(
            "empty_title",
            format!(
                "utility model `{}` returned no usable content (raw output empty after cleaning)",
                utility_spec.selection_id()
            ),
        ));
    }
    Ok(clamp_title(cleaned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_quotes_and_prefixes() {
        assert_eq!(clean_generated_title("  \"多平台总结\" "), "多平台总结");
        assert_eq!(clean_generated_title("《标题》"), "标题");
        assert_eq!(clean_generated_title("Title: News summary"), "News summary");
        assert_eq!(clean_generated_title("标题：新闻总结"), "新闻总结");
        assert_eq!(clean_generated_title("`git help`"), "git help");
    }

    #[test]
    fn clamps_long_titles_by_chars_not_bytes() {
        assert_eq!(clamp_title("短标题".to_string()), "短标题");
        let long = "a".repeat(50);
        assert_eq!(clamp_title(long).chars().count(), 40);
        // 40 CJK chars are 120 bytes — must not panic on char boundaries
        let cjk = "标".repeat(50);
        assert_eq!(clamp_title(cjk).chars().count(), 40);
    }

    #[test]
    fn truncates_excerpts_on_char_boundaries() {
        assert_eq!(truncate_chars("hello", 3), "hel");
        let cjk = "雨".repeat(10);
        assert_eq!(truncate_chars(&cjk, 4).chars().count(), 4);
        assert_eq!(truncate_chars("ab", 5), "ab");
    }

    #[tokio::test]
    async fn title_generation_without_a_utility_model_is_structured() {
        let registry = ProviderRegistry::default();
        let err = generate_title(&registry, "hi", None).await.unwrap_err();
        assert_eq!(err.kind, "no_utility_model");
        assert!(!err.detail.is_empty());
    }
}
