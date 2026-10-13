//! Per-run retention of what happened, for the hosts that persist a trace file.
//!
//! The kernel produces one [`ToolTrace`] per call and emits it; keeping the
//! records is a host's business. Both first-party hosts write the same file
//! layout (`<store>/<session>/traces/<task_id>.json`), so the retention rules
//! live here rather than once per host — two implementations of "what gets
//! dropped" would eventually disagree about what a trace file means.

use thunder_agent_loop::tools::trace::{AskAnswer, Decision, GuardHit, ToolTrace, TraceTiming};
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::policy::{Caller, ToolEffect};
use thunder_agent_loop::types::scope::Scope;

/// Keeps one run's events and tool traces, within bounds.
///
/// The bounds are the point: a long task can emit hundreds of thousands of
/// streaming deltas, and a trace file that grows without limit is a trace file
/// nobody can open. What is dropped is *counted*, so the record stays honest
/// about its own completeness.
#[derive(Debug, Default)]
pub struct TraceCollector {
    events: Vec<ObservedEvent>,
    tool_traces: Vec<ToolTrace>,
    /// Traces pushed out by the cap, reduced to what explains them: the value of
    /// a record is mostly "why was this allowed or refused", and a
    /// [`DecisionRecord`] keeps exactly that at a fraction of the size.
    evicted: Vec<DecisionRecord>,
    dropped_deltas: usize,
    dropped_events: usize,
}

/// How many non-streaming events are kept.
pub const MAX_EVENTS: usize = 5000;
/// How many full tool traces are kept.
pub const MAX_TOOL_TRACES: usize = 2000;
/// How many evicted traces keep their decision, and how many are kept.
pub const MAX_DECISION_RECORDS: usize = 2000;

impl TraceCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observed event, classifying streaming noise on the way.
    ///
    /// Micro-deltas are counted rather than kept: a run emits one per token, and
    /// nothing downstream reconstructs a transcript from them.
    pub fn record(&mut self, event: ObservedEvent) {
        let is_micro_delta = matches!(
            &event.event,
            AgentEvent::TokenDelta { .. }
                | AgentEvent::ReasoningDelta { .. }
                | AgentEvent::ToolCallChunk { .. }
        );
        if is_micro_delta {
            self.dropped_deltas += 1;
            return;
        }
        // Live `metrics_update` custom events are high-frequency telemetry
        // snapshots derived from the stream; the final snapshot is persisted at
        // the top level of the trace file, so individual updates are not kept in
        // `events` nor counted as dropped deltas.
        if matches!(
            &event.event,
            AgentEvent::Custom { kind, .. } if kind == crate::plugins::metrics::METRICS_UPDATE_EVENT_KIND
        ) {
            return;
        }
        // A tool trace arrives as an event too. It is kept as a record of its
        // own, in its own budget, so event pressure can never push a decision
        // out of the file.
        if let AgentEvent::ToolTrace { trace } = &event.event {
            self.record_tool_trace((**trace).clone());
        }
        if self.events.len() < MAX_EVENTS {
            self.events.push(event);
        } else {
            self.dropped_events += 1;
        }
    }

    /// Record one finished tool call, evicting the oldest when the cap is hit.
    pub fn record_tool_trace(&mut self, trace: ToolTrace) {
        if self.tool_traces.len() >= MAX_TOOL_TRACES {
            let oldest = self.tool_traces.remove(0);
            self.evicted.push(DecisionRecord::of(&oldest));
            if self.evicted.len() > MAX_DECISION_RECORDS {
                self.evicted.remove(0);
            }
        }
        self.tool_traces.push(trace);
    }

    pub fn events(&self) -> &[ObservedEvent] {
        &self.events
    }

    pub fn tool_traces(&self) -> &[ToolTrace] {
        &self.tool_traces
    }

    /// Traces that did not fit, as the part that explains them.
    pub fn evicted_decisions(&self) -> &[DecisionRecord] {
        &self.evicted
    }

    /// Streaming deltas that were not kept. Counted, not silently discarded:
    /// the point of the number is that a reader can tell a quiet run from one
    /// whose noise was thrown away.
    pub fn dropped_deltas(&self) -> usize {
        self.dropped_deltas
    }

    /// Non-streaming events that did not fit.
    pub fn dropped_events(&self) -> usize {
        self.dropped_events
    }

    /// Whether anything at all was dropped, for a reader deciding how much to
    /// trust the file's completeness.
    pub fn is_complete(&self) -> bool {
        self.dropped_events == 0 && self.dropped_deltas == 0 && self.evicted.is_empty()
    }
}

/// The explaining half of a tool trace, kept when the full record cannot be.
///
/// Assembled by hand rather than by cloning the trace and blanking fields: what
/// belongs here is a decision, and a field list that makes that explicit is one
/// a reader can trust.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionRecord {
    pub tool_call_id: String,
    pub turn: usize,
    pub tool: String,
    pub caller: Caller,
    pub effect: ToolEffect,
    pub mode: Option<String>,
    pub scope: Scope,
    pub decision: Decision,
    pub guard: Option<GuardHit>,
    pub answer: Option<AskAnswer>,
    pub timing: TraceTiming,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
}

impl DecisionRecord {
    /// Reduce a finished trace to the part that explains it.
    pub fn of(trace: &ToolTrace) -> Self {
        Self {
            tool_call_id: trace.tool_call_id.clone(),
            turn: trace.turn,
            tool: trace.tool.clone(),
            caller: trace.caller.clone(),
            effect: trace.effect,
            mode: trace.mode.clone(),
            scope: trace.scope,
            decision: trace.decision.clone(),
            guard: trace.guard.clone(),
            answer: trace.answer,
            timing: trace.timing,
            started_at_ms: trace.started_at_ms,
            finished_at_ms: trace.finished_at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thunder_agent_loop::tools::trace::{Decision, TraceResult, TRACE_SCHEMA};
    use thunder_agent_loop::types::event::AgentEvent;

    fn observed(event: AgentEvent) -> ObservedEvent {
        ObservedEvent {
            agent_id: "a".into(),
            event,
            timestamp_ms: 1,
        }
    }

    fn trace(n: usize) -> ToolTrace {
        ToolTrace {
            schema: TRACE_SCHEMA,
            trace_id: format!("t-{n}"),
            session_id: None,
            task_id: None,
            turn: n,
            tool_call_id: format!("c-{n}"),
            tool: "stub".into(),
            caller: Caller::Model,
            effect: ToolEffect::Read,
            mode: None,
            scope: Scope::Unknown,
            arguments: serde_json::Value::Null,
            decision: Decision::Allow,
            guard: None,
            answer: None,
            result: TraceResult {
                is_error: false,
                truncated: false,
                original_bytes: 0,
                result_bytes: 0,
                output_preview: String::new(),
                output_digest: String::new(),
            },
            redactions: Vec::new(),
            timing: TraceTiming::default(),
            started_at_ms: n as u64,
            finished_at_ms: n as u64,
        }
    }

    /// Streaming noise is counted, never kept — and the count is the record's
    /// own honesty about what it left out.
    #[test]
    fn micro_deltas_are_counted_not_kept() {
        let mut c = TraceCollector::new();
        for _ in 0..10 {
            c.record(observed(AgentEvent::TokenDelta {
                turn: 1,
                delta: "x".into(),
            }));
        }
        assert!(c.events().is_empty());
        assert_eq!(c.dropped_deltas(), 10);
        assert!(!c.is_complete(), "a run that dropped something says so");

        let mut c = TraceCollector::new();
        c.record(observed(AgentEvent::TurnStart {
            turn: 1,
            timestamp: 1,
        }));
        assert_eq!(c.events().len(), 1);
        assert!(c.is_complete());
    }

    /// A tool trace is kept in its own budget, so a storm of events cannot push
    /// a decision out of the record.
    #[test]
    fn a_tool_trace_is_kept_even_under_event_pressure() {
        let mut c = TraceCollector::new();
        for i in 0..MAX_EVENTS + 100 {
            c.record(observed(AgentEvent::TurnStart {
                turn: i,
                timestamp: i as u64,
            }));
        }
        assert_eq!(c.events().len(), MAX_EVENTS);
        assert_eq!(c.dropped_events(), 100);

        // Delivered as an event, still recorded as a trace.
        c.record(observed(AgentEvent::ToolTrace {
            trace: Box::new(trace(1)),
        }));
        assert_eq!(c.tool_traces().len(), 1);
        assert_eq!(c.tool_traces()[0].tool_call_id, "c-1");
    }

    /// Past the cap the newest records win, and what is pushed out keeps the
    /// part that explains it: the question a trace answers is "why".
    #[test]
    fn evicted_traces_keep_their_decisions() {
        let mut c = TraceCollector::new();
        for i in 0..MAX_TOOL_TRACES + 3 {
            c.record_tool_trace(trace(i));
        }
        assert_eq!(c.tool_traces().len(), MAX_TOOL_TRACES);
        assert_eq!(
            c.tool_traces()[0].tool_call_id,
            format!("c-{}", 3),
            "the oldest full records are the ones pushed out"
        );
        assert_eq!(c.evicted_decisions().len(), 3);
        assert_eq!(c.evicted_decisions()[0].tool_call_id, "c-0");
        assert_eq!(c.evicted_decisions()[0].decision, Decision::Allow);
        assert!(!c.is_complete());
    }

    /// The eviction log is bounded too: an unbounded "what we dropped" list is
    /// the same leak as an unbounded trace.
    #[test]
    fn the_eviction_log_is_bounded() {
        let mut c = TraceCollector::new();
        for i in 0..MAX_TOOL_TRACES + MAX_DECISION_RECORDS + 10 {
            c.record_tool_trace(trace(i));
        }
        assert_eq!(c.evicted_decisions().len(), MAX_DECISION_RECORDS);
    }
}
