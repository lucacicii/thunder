//! Conversation title generation for the TUI host.
//!
//! Mirrors the daemon's policy: the LLM-side generation lives in
//! `thunder-agent-providers::naming` (shared), while this module owns the
//! store round-trip and the manual-title protection.

use thunder_agent_loop::ChatMessage;
use thunder_agent_providers::naming::TitleGenError;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_conversation::prelude::{Conversation, ConversationStore, FsConversationStore};

fn first_message_text(conv: &Conversation, want_user: bool) -> Option<String> {
    for msg in &conv.messages {
        match (msg, want_user) {
            (ChatMessage::User { content, .. }, true) => return Some(content.clone()),
            (ChatMessage::Assistant { content, .. }, false) => {
                return content.clone().filter(|c| !c.trim().is_empty());
            }
            _ => continue,
        }
    }
    None
}

/// Generate (or regenerate) and persist a conversation title.
///
/// Never overwrites a manual title unless `force` is set. Returns the final
/// title or a structured error for the status line.
pub async fn generate_conversation_title(
    store: &FsConversationStore,
    registry: &ProviderRegistry,
    session_id: &str,
    force: bool,
) -> Result<String, TitleGenError> {
    let conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err(TitleGenError::new(
                "not_found",
                format!("conversation `{session_id}` not found"),
            ))
        }
        Err(e) => {
            return Err(TitleGenError::new(
                "store_error",
                format!("failed to load conversation: {e}"),
            ))
        }
    };

    if conv.is_title_manual() && !force {
        return Err(TitleGenError::new(
            "manual_locked",
            "the name was set manually; use `/rename --auto` to override",
        ));
    }

    let prompt = first_message_text(&conv, true).ok_or_else(|| {
        TitleGenError::new(
            "empty_title",
            "conversation has no user message to summarize",
        )
    })?;
    let assistant_text = first_message_text(&conv, false);

    let final_title = thunder_agent_providers::naming::generate_title(
        registry,
        &prompt,
        assistant_text.as_deref(),
    )
    .await?;

    // Re-load to avoid clobbering concurrent writes, then persist.
    let mut conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        _ => {
            return Err(TitleGenError::new(
                "store_error",
                format!("conversation `{session_id}` disappeared during title generation"),
            ))
        }
    };
    conv.title = Some(final_title.clone());
    conv.title_source = Some("auto".to_string());
    conv.updated_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    store
        .save(&conv)
        .await
        .map_err(|e| TitleGenError::new("store_error", format!("failed to persist title: {e}")))?;

    Ok(final_title)
}
