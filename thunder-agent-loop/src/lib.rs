//! # Thunder Agent Loop
//!
//! A complete single-agent unit: create, register tools, `run`/`start`, observe, cancel.
//! Another Rust app (scheduler B) may compose many units. This crate never depends on B.
//!
//! See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the A/B contract.

pub mod cache;
pub mod core;
pub mod loop_engine;
pub mod pruning;
pub mod stream;
pub mod tools;
pub mod types;

/// Stable product surface for a single agent unit and for a scheduler composing many units.
pub mod prelude {
    pub use crate::cache::warmer::{CacheWarmDecision, PromptCacheWarmSettings, WarmSnapshot};
    pub use crate::core::images::{
        base64_decode, base64_encode, decode_image_data_url, is_allowed_image_mime,
        sniff_image_mime, validate_image_bytes, MAX_IMAGES_PER_MESSAGE, MAX_IMAGE_BYTES,
    };
    pub use crate::core::pause::PauseGate;
    pub use crate::core::state::LoopStatus;
    pub use crate::core::steer::{
        PendingQueue, QueueBehavior, QueueMode, QueueSnapshot, SteerQueues,
    };
    pub use crate::loop_engine::engine::{AgentLoop, AgentRunResult, ContextInput};
    pub use crate::loop_engine::gate::{AcceptAllGate, CompletionGate, GateRequest, GateVerdict};
    pub use crate::loop_engine::handle::AgentHandle;
    pub use crate::pruning::error_detector::{
        extract_context_overflow_limit, is_context_overflow_error,
    };
    pub use crate::stream::client::{
        ChatRequestOptions, LLMClientTrait, LLMStreamChunk, UnconfiguredLLMClient,
    };
    pub use crate::tools::scratchpad::{
        Artifact, ArtifactManifest, ScratchpadConfig, ScratchpadManager,
    };
    pub use crate::tools::trace::{
        AskAnswer, Decision, DenyBy, GuardHit, GuardKind, ToolTrace, ToolTraceSink,
        TraceInterceptor, TraceResult, TraceTiming, TRACE_SCHEMA,
    };
    pub use crate::tools::Guard;
    pub use crate::types::config::{AgentConfig, ContextPruningConfig, LoopGuardConfig};
    pub use crate::types::error::AgentError;
    pub use crate::types::event::{AgentEvent, AgentStats, FinishReason, ObservedEvent, TurnStats};
    pub use crate::types::invoke::{
        empty_tool_invoker_slot, ExecutorToolInvoker, NullToolInvoker, ToolInvocationContext,
        ToolInvoker, ToolInvokerSlot,
    };
    pub use crate::types::message::{ChatMessage, ContentPart, Role, ToolCall, ToolCallFunction};
    // `GateVerdict` is deliberately not re-exported from `types::gate` here:
    // the completion gate already owns that name in this prelude, and a host
    // that wants the tool gate's verdict says so by naming the module.
    pub use crate::types::gate::{
        ApprovalAnswer, ApprovalPrompt, ToolGate, ToolGateRequest, ALLOW_ALWAYS, ALLOW_ONCE, DENY,
        DENY_WITH_REASON,
    };
    pub use crate::types::policy::{Caller, ToolEffect};
    pub use crate::types::scope::{Scope, ToolScope, UnknownScope};
    pub use crate::types::tool::{
        AgentTool, FunctionDefinition, ProgressReporter, ToolDefinition, ToolEventSink,
        ToolExecutionContext, ToolExecutionResult,
    };
    pub use crate::types::ui::{
        HostUi, NotifyLevel, NullHostUi, UiRequest, UiResponse, UiSource, DEFAULT_UI_TIMEOUT,
    };
}

pub use prelude::*;
