use crate::types::message::ToolCall;
use crate::types::tool::ToolExecutionResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnStats {
    pub turn: usize,
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<usize>,
    /// Prompt tokens freshly written into the provider's prompt cache this
    /// turn (billed at the cache-write premium). Distinct from `cached_tokens`
    /// (cache reads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<usize>,
    pub duration_ms: u64,
    pub tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Done,
    MaxTurnsExceeded,
    BudgetExceeded,
    Cancelled,
    Error,
}

/// Event tagged with the emitting unit id so a scheduler can demux many agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedEvent {
    pub agent_id: String,
    pub event: AgentEvent,
    /// Wall-clock time when the loop emitted the event. This is intentionally
    /// outside `AgentEvent` so every variant gets a uniform trace timestamp.
    #[serde(default)]
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStart {
        turn: usize,
        timestamp: u64,
    },
    /// Streaming token for user-visible answer content
    TokenDelta {
        turn: usize,
        delta: String,
    },
    /// Streaming token for chain-of-thought / reasoning (e.g. DeepSeek-R1, o1 series)
    ReasoningDelta {
        turn: usize,
        delta: String,
    },
    ToolCallChunk {
        turn: usize,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    ToolCallReady {
        turn: usize,
        tool_call: ToolCall,
    },
    ToolExecStart {
        turn: usize,
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolExecResult {
        turn: usize,
        tool_call_id: String,
        name: String,
        result: ToolExecutionResult,
    },
    TelemetryNotice {
        turn: usize,
        tool_call_id: String,
        layer: String,
        action: String,
        ground_truth: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        self_healed: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        guidance: Option<String>,
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
    /// Pi-style checkpoint compaction rewrote the older history into a single
    /// structured summary. One-time event per compaction.
    ContextCompacted {
        turn: Option<usize>,
        tokens_before: usize,
        tokens_after: usize,
    },
    /// A queued steering / follow-up message entered the transcript.
    ///
    /// Emitted at the turn boundary, at the same moment the message joins the
    /// context, so a host can move it out of its "pending" display and into the
    /// transcript without guessing when the loop picked it up.
    SteerAccepted {
        turn: usize,
        /// `steer` or `follow_up`.
        behavior: String,
        message: String,
        /// How many images rode along. The bytes stay in the engine: an event
        /// carrier is the wrong place for base64, and the host only needs to
        /// know there was more than text.
        #[serde(default)]
        image_count: usize,
    },
    Error {
        turn: Option<usize>,
        message: String,
        recoverable: bool,
    },
    /// A host-defined event, forwarded verbatim.
    ///
    /// The loop cannot know that "two files changed" or "the form failed
    /// validation" deserves an event, so it does not try. A tool or middleware
    /// emits one through the execution context's event sink, and the loop
    /// carries it here without interpreting `kind` or `payload`. Hosts pattern
    /// match on their own `kind`; unknown kinds are safe to ignore.
    Custom {
        kind: String,
        payload: serde_json::Value,
    },
    /// The completion gate judged a finished answer.
    ///
    /// Emitted once per gate evaluation so a host can render "verifying..."
    /// and, on `retry`, explain why the run did not stop.
    GateResult {
        turn: usize,
        /// 1-based gate evaluation counter within this run.
        round: usize,
        /// `pass`, `retry`, or `fail`.
        verdict: String,
        /// The gate's feedback (retry) or failure reason (fail).
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_event_timestamp_defaults_for_old_traces() {
        let event: ObservedEvent = serde_json::from_value(serde_json::json!({
            "agent_id": "agent-1",
            "event": { "type": "turn_start", "turn": 1, "timestamp": 42 }
        }))
        .expect("old events deserialize");

        assert_eq!(event.timestamp_ms, 0);
    }
}
