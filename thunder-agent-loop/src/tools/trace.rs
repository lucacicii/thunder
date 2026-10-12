//! One record per tool call, however the call ends.
//!
//! The trace is the kernel's answer to "what actually happened, and who decided
//! it". It is produced by the single tool-execution wrapper
//! ([`crate::tools::executor::ToolExecutor`]) rather than by any layer inside it,
//! so a call that is refused by the guard, refused by the gate, cancelled,
//! timed out, or run to completion all leave exactly one record. There is no way
//! for a tool to run untraced.
//!
//! The record carries the *decision*, not just the outcome: which scope the call
//! landed in, whether the guard intervened, what the user answered, and how long
//! the user took to answer. That is what makes a refused call explainable after
//! the fact — the question the onion's telemetry could not answer.
//!
//! Redaction belongs here too. A trace is a durable artifact that outlives the
//! conversation and travels to disk, so credentials are stripped on the way in;
//! the model's own context is left alone, because a model that cannot see the
//! truth cannot work. See [`ToolTrace::arguments`] and
//! [`TraceResult::output_preview`].

use crate::types::policy::{Caller, ToolEffect};
use crate::types::scope::Scope;
use serde::{Deserialize, Serialize};

/// Version of the trace layout. Bumped when a field changes meaning, so a reader
/// can tell an old record from a new one.
pub const TRACE_SCHEMA: u8 = 1;

/// Everything that happened to one tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolTrace {
    /// [`TRACE_SCHEMA`] at the time the record was written.
    pub schema: u8,
    /// Unique per call, so a trace can be joined against a host's own log.
    pub trace_id: String,
    /// The host's session, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The host's task, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Which turn of the loop issued the call.
    pub turn: usize,
    /// The provider's tool call id, matching `ToolExecStart`/`ToolExecResult`.
    pub tool_call_id: String,
    /// The registered tool name.
    pub tool: String,
    /// Who asked: the model, or a plugin on its own initiative.
    pub caller: Caller,
    /// What the call could change.
    pub effect: ToolEffect,
    /// The gate's own label for the state it was in — a mode name, in the usual
    /// case. The kernel stores it verbatim and never interprets it; absent when
    /// no gate was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Where the call landed, from the host's scope (or
    /// [`Scope::Unknown`](crate::types::scope::Scope::Unknown) without one).
    pub scope: Scope,
    /// The arguments the tool received, **after redaction**.
    pub arguments: serde_json::Value,
    /// What was decided, and by whom.
    pub decision: Decision,
    /// The guard's intervention, when there was one. Present only on a guard
    /// refusal — a call the guard passed shows `None`, not a "no hit" object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<GuardHit>,
    /// The user's answer, when the call was asked about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AskAnswer>,
    /// What came back — for a refusal, the refusal text the model saw.
    pub result: TraceResult,
    /// Redaction rules that actually fired, by name, so a reader can tell
    /// "there was no secret" from "a secret was removed".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
    /// Where the wall-clock time went.
    pub timing: TraceTiming,
    /// When the call entered the wrapper.
    pub started_at_ms: u64,
    /// When the wrapper finished with it.
    pub finished_at_ms: u64,
}

/// What the wrapper concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Allowed, with no standing rule involved.
    Allow,
    /// Allowed because a rule the user had already agreed to matched.
    AllowByRule {
        /// The rule's label, as the gate described it.
        rule: String,
    },
    /// Asked the user. What they said is in [`ToolTrace::answer`] — including
    /// the case where they never said anything and the call was refused.
    Asked,
    /// Refused without asking.
    Deny {
        /// What the model was told.
        reason: String,
        /// Which side refused.
        by: DenyBy,
    },
    /// The call's cancellation token fired before it could run.
    Cancelled,
}

/// Which side of the kernel refused a call.
///
/// Worth distinguishing: a guard refusal is the kernel's unconditional floor and
/// holds in every mode, while a gate refusal is a policy the host chose and can
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyBy {
    /// The kernel's guard: a dangerous command or a critical path.
    Guard,
    /// The installed gate: the host's policy.
    Gate,
}

/// A guard's refusal, in enough detail to audit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardHit {
    /// Which list the call matched.
    pub kind: GuardKind,
    /// The rule that matched, e.g. `rm -rf /` or `.git`.
    pub pattern: String,
    /// The part of the call that matched, for a reader deciding whether the rule
    /// is too broad.
    pub detail: String,
}

/// The kinds of unconditional refusal the kernel keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardKind {
    /// A shell command matched the destructive-command list.
    DangerousCommand,
    /// A target resolved into a directory or file the kernel protects.
    CriticalPath,
}

/// What the user said, when they were asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskAnswer {
    /// Allowed this call.
    AllowOnce,
    /// Allowed it and agreed to a standing rule.
    AllowAlways,
    /// Refused.
    Denied,
    /// The dialog went unanswered; the call was refused.
    Timeout,
    /// No host UI was attached; the call was refused.
    NoUi,
    /// The host answered with something the kernel does not recognise; the call
    /// was refused.
    Unrecognised,
}

/// What came back from the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceResult {
    /// Whether the tool reported a failure.
    pub is_error: bool,
    /// Whether the output was cut down before reaching the model.
    pub truncated: bool,
    /// Size of the tool's full output, before any truncation.
    pub original_bytes: usize,
    /// Size of what the model actually received.
    pub result_bytes: usize,
    /// The head of the output, redacted. Not the whole output: the full text is
    /// already kept losslessly by the scratchpad spill, and a trace that embeds
    /// every megabyte is a trace nobody reads.
    pub output_preview: String,
    /// [`stable_digest`] of the full, *unredacted* output — enough to tell
    /// whether two calls produced the same thing without keeping the bytes.
    pub output_digest: String,
}

/// Where the time went, in milliseconds.
///
/// `total_ms` is not necessarily the sum: a call may wait behind the dialog
/// serialisation, and a refusal never reaches the tool at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TraceTiming {
    /// Waiting for the kernel's turn to run.
    #[serde(default)]
    pub queued_ms: u64,
    /// The guard, scope and gate, together.
    #[serde(default)]
    pub decision_ms: u64,
    /// Time spent waiting on the user.
    #[serde(default)]
    pub ask_ms: u64,
    /// The tool itself.
    #[serde(default)]
    pub exec_ms: u64,
    /// Wrapper entry to exit.
    #[serde(default)]
    pub total_ms: u64,
}

/// A host's sink for finished traces.
///
/// One direction only: the kernel hands over a record and forgets it. The record
/// arrives serialised and redacted, so a sink that writes it to disk never has to
/// know which fields were sensitive — one redaction implementation, not one per
/// host.
pub trait ToolTraceSink: Send + Sync {
    /// Take one finished trace.
    ///
    /// Must not block the tool path for long, and must not fail the call: a sink
    /// that cannot keep up loses records, it does not stop work.
    fn record(&self, trace: &ToolTrace);
}

/// A digest of `bytes` that is stable across releases: FNV-1a, 64-bit, hex.
///
/// Stability is the whole requirement — two runs over the same content must agree
/// after a version upgrade, which rules out `std`'s `DefaultHasher` (documented
/// as unstable between releases). This is for dedupe and "did this change?"
/// checks, **not** for authenticity; nothing here resists a deliberate collision.
pub fn stable_digest(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_trace() -> ToolTrace {
        ToolTrace {
            schema: TRACE_SCHEMA,
            trace_id: "t-1".into(),
            session_id: Some("s-1".into()),
            task_id: None,
            turn: 2,
            tool_call_id: "call_1".into(),
            tool: "write_file".into(),
            caller: Caller::Model,
            effect: ToolEffect::Write,
            mode: Some("ask".into()),
            scope: Scope::OutOfScope,
            arguments: json!({ "path": "/tmp/x" }),
            decision: Decision::Asked,
            guard: None,
            answer: Some(AskAnswer::AllowOnce),
            result: TraceResult {
                is_error: false,
                truncated: false,
                original_bytes: 3,
                result_bytes: 3,
                output_preview: "ok\n".into(),
                output_digest: stable_digest(b"ok\n"),
            },
            redactions: vec![],
            timing: TraceTiming {
                total_ms: 1,
                ..Default::default()
            },
            started_at_ms: 1_700_000_000_000,
            finished_at_ms: 1_700_000_000_001,
        }
    }

    #[test]
    fn a_trace_carries_the_decision_and_the_answer_apart() {
        let trace = sample_trace();
        assert_eq!(trace.decision, Decision::Asked);
        assert_eq!(trace.answer, Some(AskAnswer::AllowOnce));
        assert_eq!(trace.scope, Scope::OutOfScope);
    }

    #[test]
    fn a_guard_refusal_names_its_side() {
        let mut trace = sample_trace();
        trace.decision = Decision::Deny {
            reason: "refused".into(),
            by: DenyBy::Guard,
        };
        trace.guard = Some(GuardHit {
            kind: GuardKind::DangerousCommand,
            pattern: "rm -rf /".into(),
            detail: "rm -rf /".into(),
        });
        trace.answer = None;

        let wire = serde_json::to_value(&trace).unwrap();
        assert_eq!(wire["decision"]["by"], json!("guard"));
        assert_eq!(wire["guard"]["kind"], json!("dangerous_command"));
        assert!(wire.get("answer").is_none(), "absent answers are omitted");
    }

    #[test]
    fn round_trips_through_the_wire() {
        let trace = sample_trace();
        let wire = serde_json::to_string(&trace).unwrap();
        let back: ToolTrace = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, trace);
    }

    #[test]
    fn optional_fields_may_be_missing_in_an_older_record() {
        let wire = json!({
            "schema": 1,
            "trace_id": "t-1",
            "turn": 1,
            "tool_call_id": "call_1",
            "tool": "bash",
            "caller": { "caller": "model" },
            "effect": "exec",
            "scope": "unknown",
            "arguments": {},
            "decision": { "decision": "allow" },
            "result": {
                "is_error": false,
                "truncated": false,
                "original_bytes": 0,
                "result_bytes": 0,
                "output_preview": "",
                "output_digest": ""
            },
            "timing": {},
            "started_at_ms": 0,
            "finished_at_ms": 0
        });

        let trace: ToolTrace = serde_json::from_value(wire).unwrap();
        assert_eq!(trace.session_id, None);
        assert_eq!(trace.mode, None);
        assert_eq!(trace.timing.total_ms, 0);
    }

    #[test]
    fn the_digest_is_stable_and_discriminating() {
        assert_eq!(stable_digest(b"ok\n"), stable_digest(b"ok\n"));
        assert_ne!(stable_digest(b"ok\n"), stable_digest(b"ok"));
        assert_eq!(
            stable_digest(b""),
            "cbf29ce484222325",
            "the FNV-1a offset basis, so a change to the algorithm is caught"
        );
    }
}
