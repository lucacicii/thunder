//! Thunder Core — Microkernel ReAct Agent Loop, Context & Checkpoint Pruning, Cache Warmer, Conversation Store, Middleware Pipeline, and KV-Cache Shield Gateway.

pub mod agent_loop;
pub mod cache_warmer;
pub mod config;
pub mod context;
pub mod conversation;
pub mod gateway;
pub mod instructions;
pub mod llm;
pub mod loop_guard;
pub mod middleware;
pub mod pause;
pub mod pruning;
pub mod read_only;
pub mod steer;
pub mod token_estimator;

pub use agent_loop::{AgentLoopConfig, AgentLoopOutcome, ReActAgentLoop};
pub use cache_warmer::{CacheWarmDecision, WarmSnapshot};
pub use config::{AgentSection, SandboxSection, ThunderConfig};
pub use context::{ContextBuffer, ContextPruningConfig, MessageEntry};
pub use conversation::{
    now_ms, provisional_title, Conversation, ConversationError, ConversationFilter,
    ConversationStats, ConversationStatsReport, ConversationStatus, ConversationStore,
    ConversationSummary, FsConversationStore, MemoryConversationStore, OrchestrationMeta,
    StageRecord,
};
pub use gateway::{
    call_tool_descriptor, discover_tools_descriptor, KvCacheShield, META_CALL_TOOL,
    META_DISCOVER_TOOLS,
};
pub use instructions::{
    discover_context_files, find_context_file_in_dir, format_instructions_section,
    ContextInstructionFile, CANDIDATE_FILENAMES,
};
pub use llm::{
    LlmDriver, LlmTurnResponse, ProviderClientDriver, ScriptedMockDriver, ToolCallRequest,
};
pub use loop_guard::{ActionRecord, LoopGuard};
pub use middleware::{
    dangerous_command, MiddlewarePipeline, MiddlewareVerdict, PlanModeMiddleware,
    SafetyGuardMiddleware, ToolMiddleware,
};
pub use pause::PauseGate;
pub use pruning::{
    extract_context_overflow_limit, is_context_overflow_error, ContextPruner, PruneResult,
    Summarizer, CHECKPOINT_MESSAGE_NAME,
};
pub use read_only::{call_is_read_only, is_read_only};
pub use steer::{PendingQueue, QueueBehavior, QueueMode, QueueSnapshot, SteerQueues};
pub use thunder_protocol::{
    AgentEvent, AgentStats, ChatMessage, ContentPart, FinishReason, ObservedEvent,
    PromptCacheWarmSettings, Role, ToolCall, ToolExecResultPayload,
};
pub use token_estimator::{estimate_image_tokens, estimate_token_count};
