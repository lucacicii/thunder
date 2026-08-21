//! # Thunder Agent Loop
//!
//! Ultra-lightweight, high-performance, minimal-resource Agent Loop Engine in Rust.

pub mod core;
pub mod loop_engine;
pub mod pruning;
pub mod stream;
pub mod tools;
pub mod types;

pub mod prelude {
    pub use crate::core::context::ContextBuffer;
    pub use crate::core::state::{AgentStateTracker, LoopStatus};
    pub use crate::core::token_estimator::estimate_token_count;
    pub use crate::loop_engine::engine::{AgentLoop, AgentRunResult, ContextInput};
    pub use crate::loop_engine::hooks::AgentEventDispatcher;
    pub use crate::pruning::strategy::{ContextPruner, PruneResult};
    pub use crate::stream::client::{ChatRequestOptions, LLMClient, LLMClientTrait, LLMStreamChunk};
    pub use crate::tools::builtin::{BashTool, ReadFileTool, WriteFileTool};
    pub use crate::tools::executor::{ExecutedToolResult, ToolExecutor};
    pub use crate::tools::registry::ToolRegistry;
    pub use crate::types::config::{AgentConfig, ContextPruningConfig, PruningStrategy};
    pub use crate::types::event::{AgentEvent, AgentStats, FinishReason, TurnStats};
    pub use crate::types::message::{ChatMessage, Role, ToolCall, ToolCallFunction};
    pub use crate::types::tool::{AgentTool, FunctionDefinition, ToolDefinition, ToolExecutionContext, ToolExecutionResult};
}

pub use prelude::*;
