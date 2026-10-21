//! The durable record of one tool call.
//!
//! Data only: a host persists these, a UI renders them, a test asserts on them.
//! The machinery that *produces* a record (the interceptor, the sink, the
//! redaction pass) lives in the loop crate.

use serde::{Deserialize, Serialize};

use crate::policy::{Caller, ToolEffect};
use crate::scope::Scope;

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
    /// The host's name for this run — the daemon's task id, the TUI's run id.
    ///
    /// The kernel only knows this as the `route` it was configured with, and
    /// never interprets it; it is recorded so a trace can be joined against the
    /// host's own record of the same run. Both first-party hosts pass the same
    /// string they file the trace under, so the id inside the file is the id of
    /// the file.
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
    /// [`Scope::Unknown`](crate::scope::Scope::Unknown) without one).
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

// ─────────────────────────────── redaction ───────────────────────────────
//
// A trace is a durable artifact: it outlives the conversation and lands on
// disk, where it may be read by someone who never saw the session. The model's
// own context is deliberately *not* filtered this way — a model that cannot see
// the truth cannot work — so this is the one place secrets are removed, and it
// runs on the way into the record.

/// A host's sink for finished traces.
///
/// One direction only: the kernel hands over a record and forgets it. The record
/// arrives serialised and redacted, so a sink that writes it to disk never has to
/// know which fields were sensitive — one redaction implementation, not one per
/// host.
pub trait ToolTraceSink: Send + Sync {
    fn record(&self, trace: &ToolTrace);
}
