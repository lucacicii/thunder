//! # Thunder Conversation
//!
//! High-performance conversation and session management for the Thunder Agent ecosystem.
//!
//! Provides:
//! - Multi-turn conversation models with Token estimation and Turn grouping
//! - Pluggable storage abstraction (`ConversationStore`) with Memory and Fs (file-system) backends
//! - Multi-agent orchestration tracking (Sequential pipeline stages, Parallel branches, Delegate tasks)
//! - Seamless bridge to `thunder-agent-loop` (`ContextInput` / `ContextBuffer`)
//! - Rich exports (Markdown, JSON, OpenAI message formats)

pub mod bridge;
pub mod error;
pub mod exporter;
pub mod manager;
pub mod orchestration;
pub mod store;
pub mod turn;
pub mod types;

pub mod prelude {
    pub use crate::error::ConversationError;
    pub use crate::exporter::ConversationExporter;
    pub use crate::manager::ConversationManager;
    pub use crate::orchestration::{OrchestrationHelper, OrchestrationTopology};
    pub use crate::store::{ConversationStore, FsConversationStore, MemoryConversationStore};
    pub use crate::turn::{extract_turns, truncate_turns, Turn};
    pub use crate::types::{
        now_ms, Conversation, ConversationFilter, ConversationStats, ConversationStatsReport,
        ConversationStatus, ConversationSummary, OrchestrationMeta, StageRecord,
    };
}

pub use prelude::*;
