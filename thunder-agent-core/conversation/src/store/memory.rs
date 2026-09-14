use super::ConversationStore;
use crate::error::ConversationError;
use crate::types::{Conversation, ConversationFilter, ConversationSummary};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default)]
pub struct MemoryConversationStore {
    conversations: Arc<RwLock<HashMap<String, Conversation>>>,
}

impl MemoryConversationStore {
    pub fn new() -> Self {
        Self {
            conversations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn count(&self) -> usize {
        self.conversations.read().await.len()
    }

    pub async fn clear(&self) {
        self.conversations.write().await.clear();
    }
}

#[async_trait]
impl ConversationStore for MemoryConversationStore {
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
        let mut lock = self.conversations.write().await;
        lock.insert(conversation.id.clone(), conversation.clone());
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Option<Conversation>, ConversationError> {
        let lock = self.conversations.read().await;
        Ok(lock.get(id).cloned())
    }

    async fn delete(&self, id: &str) -> Result<bool, ConversationError> {
        let mut lock = self.conversations.write().await;
        Ok(lock.remove(id).is_some())
    }

    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError> {
        let lock = self.conversations.read().await;
        let mut summaries: Vec<ConversationSummary> = lock
            .values()
            .map(|c| c.to_summary())
            .filter(|s| filter.matches(s))
            .collect();

        // Sort descending by updated_at_ms
        summaries.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));

        if let Some(offset) = filter.offset {
            if offset < summaries.len() {
                summaries = summaries.split_off(offset);
            } else {
                summaries.clear();
            }
        }

        if let Some(limit) = filter.limit {
            summaries.truncate(limit);
        }

        Ok(summaries)
    }

    async fn exists(&self, id: &str) -> Result<bool, ConversationError> {
        let lock = self.conversations.read().await;
        Ok(lock.contains_key(id))
    }
}
