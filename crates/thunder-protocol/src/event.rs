use crate::message::{ToolCall, TurnStats};
use crate::schema::ToolExecutionResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentStats {
    pub total_turns: usize,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    #[serde(default)]
    pub total_cached_tokens: usize,
    #[serde(default)]
    pub total_reasoning_tokens: usize,
    pub total_duration_ms: u64,
    pub total_tool_executions: usize,
    pub total_tool_time_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tokens_per_second: Option<f64>,
    /// Number of turns that produced a new state-change digest.
    #[serde(default)]
    pub mutations: usize,
    /// Number of tool results that were new information (first time seen).
    #[serde(default)]
    pub novel_observations: usize,
    /// Number of turns closed with a stall signal.
    #[serde(default)]
    pub stall_events: usize,
    /// Highest "no new tool result" streak reached in this run. Compare it against the
    /// configured threshold to tune it: a run that peaked at 2 is far from a limit of 6,
    /// one that peaked at 5 means the limit is tight.
    #[serde(default)]
    pub peak_no_novel_streak: usize,
    /// Highest "no state mutation, and nothing new learned" streak reached in this run.
    #[serde(default)]
    pub peak_mutation_free_streak: usize,
    /// The authoritative reason the loop ended, kept even when the wire-level
    /// `LoopComplete.finish_reason` reports `Done` because a wrap-up summary was
    /// produced (see `docs/agent-loop-budget-and-recovery.md` §3.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Done,
    MaxTurnsExceeded,
    BudgetExceeded,
    Cancelled,
    Error,
}

/// Wire-compatible tool execution summary inside `AgentEvent::ToolExecResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecResultPayload {
    pub tool_call_id: String,
    pub output: String,
    pub is_error: bool,
    pub truncated: bool,
    pub original_bytes: usize,
    pub duration_ms: u64,
}

/// Fine-grained streaming event compatible with `agent-resume-panel` (`ThunderAgentEvent`) and `thunder-tui`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStart {
        turn: usize,
        timestamp: u64,
        #[serde(default)]
        estimated_prompt_tokens: usize,
    },
    TokenDelta {
        turn: usize,
        delta: String,
    },
    ReasoningDelta {
        turn: usize,
        delta: String,
    },
    ToolCallReady {
        turn: usize,
        tool_call: ToolCall,
    },
    ToolExecStart {
        turn: usize,
        tool_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolExecResult {
        turn: usize,
        tool_call_id: String,
        name: String,
        result: ToolExecResultPayload,
    },
    TurnEnd {
        turn: usize,
        finish_reason: String,
        stats: TurnStats,
    },
    LoopComplete {
        finish_reason: FinishReason,
        final_content: Option<String>,
        stats: AgentStats,
    },
    ContextCompacted {
        turn: Option<usize>,
        tokens_before: usize,
        tokens_after: usize,
    },
    SteerAccepted {
        turn: usize,
        behavior: String,
        message: String,
        #[serde(default)]
        image_count: usize,
    },
    Error {
        turn: Option<usize>,
        message: String,
        recoverable: bool,
    },
    Custom {
        kind: String,
        payload: Value,
    },
}

/// Event tagged with `agent_id` and `timestamp_ms` for daemon / TUI demultiplexing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedEvent {
    pub agent_id: String,
    pub event: AgentEvent,
    #[serde(default)]
    pub timestamp_ms: u64,
}

/// Event bus enumeration emitted across the Thunder microkernel (§2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThunderEvent {
    /// Emitted at the start of an inference turn after freezing the `CatalogSnapshot`.
    TurnStarted { turn: usize, snapshot_version: u64 },
    /// Emitted when hot-reload updates the `CatalogSnapshot`.
    RegistryUpdated {
        version: u64,
        added_tools: Vec<String>,
        shadowed_tools: Vec<String>,
    },
    /// Emitted before executing a tool call (direct or via `thunder__call_tool`).
    ToolCallDispatched {
        call_id: String,
        tool_name: String,
        via_gateway: bool,
    },
    /// Emitted after a tool call finishes and is normalized.
    ToolCallCompleted {
        call_id: String,
        tool_name: String,
        result: ToolExecutionResult,
        duration_ms: u64,
    },
    /// Emitted when a system-level fault (`ToolExecutionResult::SystemFault`) occurs.
    SystemFaultDetected { tool_name: String, reason: String },
    /// Emitted when the ReAct Agent Loop completes.
    AgentFinished {
        turns: usize,
        final_response: String,
    },
}

/// Sink for observing [`ThunderEvent`] and fine-grained [`AgentEvent`] notifications.
pub trait EventSink: Send + Sync {
    fn on_event(&self, event: ThunderEvent);

    fn on_agent_event(&self, _event: AgentEvent) {}
}
