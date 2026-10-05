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
    ///
    /// Conversations without a single user turn are **not** persisted: the call
    /// is a no-op and returns `Ok(())`. A host seeds a system prompt and may
    /// append an assistant greeting before the user has typed anything, and
    /// that state must never turn into a session row in a listing or on disk.
    /// Durability starts with the first user turn.
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
