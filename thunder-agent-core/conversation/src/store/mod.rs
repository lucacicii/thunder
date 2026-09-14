pub mod fs;
pub mod memory;

use crate::error::ConversationError;
use crate::types::{Conversation, ConversationFilter, ConversationSummary};
use async_trait::async_trait;

pub use fs::FsConversationStore;
pub use memory::MemoryConversationStore;

#[async_trait]
pub trait ConversationStore: Send + Sync {
    /// Save or update a conversation.
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError>;

    /// Load a full conversation by its unique ID.
    async fn load(&self, id: &str) -> Result<Option<Conversation>, ConversationError>;

    /// Delete a conversation by its unique ID. Returns true if it existed and was deleted.
    async fn delete(&self, id: &str) -> Result<bool, ConversationError>;

    /// List conversation summaries matching the given filter.
    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError>;

    /// Check whether a conversation exists.
    async fn exists(&self, id: &str) -> Result<bool, ConversationError> {
        Ok(self.load(id).await?.is_some())
    }
}
