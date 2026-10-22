use crate::heuristic::generate_heuristic_suggestions;
use crate::llm_suggest::generate_llm_suggestions;
use std::sync::Arc;
use thunder_protocol::{ChatMessage, LLMClientTrait, Role};

#[derive(Clone, Default)]
pub struct SuggestionEngine {
    client: Option<Arc<dyn LLMClientTrait>>,
    model: Option<String>,
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_client(
        mut self,
        client: Arc<dyn LLMClientTrait>,
        model: impl Into<String>,
    ) -> Self {
        self.client = Some(client);
        self.model = Some(model.into());
        self
    }

    /// Generate 1-3 actionable follow-up suggestions for the conversation.
    ///
    /// Fast-path: Uses zero-latency heuristic pattern matching on touched tools and context.
    /// If an LLM client is configured and heuristic returns default fallbacks, attempts an
    /// LLM-generated suggestion pass with a 4s timeout before falling back.
    pub async fn generate_suggestions(&self, messages: &[ChatMessage]) -> Vec<String> {
        let heuristic = generate_heuristic_suggestions(messages);
        if let (Some(ref client), Some(ref model)) = (&self.client, &self.model) {
            let last_user = messages
                .iter()
                .rev()
                .find(|m| m.role() == Role::User)
                .and_then(|m| m.content_str())
                .unwrap_or("");
            let last_assistant = messages
                .iter()
                .rev()
                .find(|m| m.role() == Role::Assistant)
                .and_then(|m| m.content_str())
                .unwrap_or("");

            if !last_user.is_empty() && !last_assistant.is_empty() {
                if let Ok(llm_items) =
                    generate_llm_suggestions(client.as_ref(), model, last_user, last_assistant)
                        .await
                {
                    if !llm_items.is_empty() {
                        return llm_items;
                    }
                }
            }
        }
        heuristic
    }
}
