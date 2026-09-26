//! # Thunder Agent Loop
//!
//! A complete single-agent unit: create, register tools, `run`/`start`, observe, cancel.
//! Another Rust app (scheduler B) may compose many units. This crate never depends on B.
//!
//! See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the A/B contract.

pub mod core;
pub mod loop_engine;
pub mod pruning;
pub mod stream;
pub mod tools;
pub mod types;

/// Stable product surface for a single agent unit and for a scheduler composing many units.
pub mod prelude {
    pub use crate::core::pause::PauseGate;
    pub use crate::core::state::LoopStatus;
    pub use crate::loop_engine::engine::{AgentLoop, AgentRunResult, ContextInput};
    pub use crate::loop_engine::handle::AgentHandle;
    pub use crate::pruning::error_detector::{
        extract_context_overflow_limit, is_context_overflow_error,
    };
    pub use crate::stream::client::{
        ChatRequestOptions, LLMClientTrait, LLMStreamChunk, UnconfiguredLLMClient,
    };
    pub use crate::tools::builtin::{BashTool, ReadFileTool, WriteFileTool};
    pub use crate::tools::scratchpad::{
        Artifact, ArtifactManifest, ScratchpadConfig, ScratchpadManager,
    };
    pub use crate::types::config::{
        AgentConfig, ContextPruningConfig, LoopGuardConfig, Permission,
        DEFAULT_AUTONOMOUS_SYSTEM_PROMPT,
    };
    pub use crate::types::error::AgentError;
    pub use crate::types::event::{AgentEvent, AgentStats, FinishReason, ObservedEvent, TurnStats};
    pub use crate::types::message::{ChatMessage, Role, ToolCall, ToolCallFunction};
    pub use crate::types::tool::{
        AgentTool, FunctionDefinition, ToolDefinition, ToolExecutionContext, ToolExecutionResult,
    };
}

pub use prelude::*;
