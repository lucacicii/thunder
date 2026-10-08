//! Host-supplied completion gate.
//!
//! The loop stops when the model stops calling tools. That is the right default
//! for a chat, and the wrong one for any task with a definition of done the
//! model cannot check itself: a prototype must still render, a form must still
//! validate, a build must still pass.
//!
//! A gate is exactly that missing judgement, stated once. When the model tries
//! to finish, the loop asks the gate; a `retry` re-enters the loop with the
//! gate's feedback as a user message, a `pass` lets the run end. The kernel
//! stays ignorant of *what* is being verified — that is the host's business,
//! and it is why this composes with any workflow, not just code.

use crate::types::event::AgentStats;
use crate::types::message::ChatMessage;
use async_trait::async_trait;

/// Everything a gate needs to judge a finished run.
#[derive(Debug, Clone)]
pub struct GateRequest {
    /// The turn that produced the answer the gate is judging.
    pub turn: usize,
    /// 1-based gate evaluation counter within this run (increments per retry).
    pub round: usize,
    /// The answer the model just produced, for a gate that can judge text alone.
    pub final_text: String,
    /// The full projected transcript, for a gate that needs to look at what the
    /// run actually did rather than only what it said.
    pub messages: Vec<ChatMessage>,
    /// Stats as of this evaluation.
    pub stats: AgentStats,
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// The run is genuinely done; let it finish.
    Pass,
    /// Not done. Re-enter the loop with `feedback` delivered to the model as a
    /// user message, so it can fix what the gate found.
    Retry { feedback: String },
    /// Not done, and not fixable by another turn. Ends the run as an error so
    /// the caller does not mistake it for success.
    Fail { reason: String },
}

/// Judges whether a finished run may actually finish.
///
/// Implementations must be cheap to hold and safe to call from the loop's task.
/// A gate that blocks on external work (a browser check, a test suite) should do
/// so inside `verify`; the loop awaits it like any other step.
#[async_trait]
pub trait CompletionGate: Send + Sync {
    async fn verify(&self, request: GateRequest) -> GateVerdict;
}

/// The default: no gate, every answer is accepted.
///
/// Exists so callers can build a gate slot unconditionally instead of threading
/// an `Option` through their own configuration.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcceptAllGate;

#[async_trait]
impl CompletionGate for AcceptAllGate {
    async fn verify(&self, _request: GateRequest) -> GateVerdict {
        GateVerdict::Pass
    }
}
