use crate::message::ToolCall;
use crate::tool::ToolExecutionResult;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// Delivery losses the engine accepted rather than stall the loop.
///
/// Micro-deltas (`TokenDelta`/`ReasoningDelta`/`ToolCallChunk`) are dropped by
/// `try_send` when the consumer is behind; structural lifecycle events are
/// dropped only after a bounded wait (see the emitter). Both used to vanish
/// silently — a saturated consumer could lose `TurnEnd`/`LoopComplete`/traces
/// with no party able to tell. The counts make that visible: hosts should
/// treat `structural() > 0` as "the run's record may be incomplete".
#[derive(Debug, Default)]
pub struct EventDropCounters {
    micro_deltas: AtomicU64,
    structural: AtomicU64,
}

impl EventDropCounters {
    /// Share one counter set between an emitter, its sinks, and the host.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Streaming micro-deltas discarded because the channel was full.
    pub fn micro_deltas(&self) -> u64 {
        self.micro_deltas.load(Ordering::Relaxed)
    }

    /// Structural events discarded after the delivery deadline.
    pub fn structural(&self) -> u64 {
        self.structural.load(Ordering::Relaxed)
    }

    /// Either kind of loss.
    pub fn total(&self) -> u64 {
        self.micro_deltas() + self.structural()
    }

    pub fn record_micro(&self) {
        self.micro_deltas.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_structural(&self) {
        self.structural.fetch_add(1, Ordering::Relaxed);
    }
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
        /// Estimated size of the prompt this turn is about to send — the whole
        /// thing, system prompt and tool definitions included.
        ///
        /// Carried so a telemetry consumer can report context occupancy (and the
        /// cost the request is about to incur) from a turn's first frame, rather
        /// than waiting for the provider's usage report at its end. Estimated,
        /// and replaced by the exact figure at `TurnEnd`.
        #[serde(default)]
        estimated_prompt_tokens: usize,
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
    /// One tool call's trace, emitted for every call however it ended.
    ///
    /// Not routed through [`AgentEvent::Custom`]: `Custom` is opaque to the
    /// kernel by definition, and this is the kernel's own record — a host has to
    /// be able to read the fields to persist, filter or render them.
    ///
    /// Boxed: a trace dwarfs every other variant, and events are cloned once per
    /// subscriber.
    ToolTrace {
        trace: Box<crate::trace::ToolTrace>,
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

    #[test]
    fn a_tool_trace_event_rides_under_its_own_tag() {
        // A trace is the largest thing on this wire, so it gets its own typed
        // variant rather than a `Custom` payload. Hosts written before it exists
        // simply never see one.
        let wire = serde_json::json!({
            "type": "tool_trace",
            "trace": {
                "schema": 1,
                "trace_id": "t-1",
                "turn": 1,
                "tool_call_id": "call_1",
                "tool": "bash",
                "caller": { "caller": "model" },
                "effect": "exec",
                "scope": "unknown",
                "arguments": { "command": "ls" },
                "decision": { "decision": "allow" },
                "result": {
                    "is_error": false,
                    "truncated": false,
                    "original_bytes": 3,
                    "result_bytes": 3,
                    "output_preview": "ok\n",
                    "output_digest": "cbf29ce484222325"
                },
                "timing": {},
                "started_at_ms": 7,
                "finished_at_ms": 8
            }
        });

        let event: AgentEvent = serde_json::from_value(wire).expect("traces deserialize");
        let AgentEvent::ToolTrace { trace } = &event else {
            panic!("expected a tool trace, got {event:?}");
        };
        assert_eq!(trace.tool, "bash");

        let back = serde_json::to_value(&event).unwrap();
        assert_eq!(back["type"], serde_json::json!("tool_trace"));
        assert_eq!(back["trace"]["effect"], serde_json::json!("exec"));
    }
}
