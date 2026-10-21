//! The kernel's one policy hook.
//!
//! The kernel has no opinion about what may run. It knows how to *ask*, how to
//! run, and how to record; who decides is a [`ToolGate`], supplied by the host
//! and removable without touching the loop. With no gate installed every call
//! runs (the guard aside), which is exactly the contract the headless runtime
//! has always relied on.
//!
//! Two properties are the kernel's, not the gate's, and no gate can weaken them:
//!
//! 1. **The guard runs first and is not consulted.** Calls that are refused
//!    unconditionally — destructive shell commands, critical paths — never reach
//!    a gate, so a gate that returns [`GateVerdict::Allow`] for everything still
//!    cannot run `rm -rf /`.
//! 2. **The dialog is the kernel's.** A gate returns [`GateVerdict::Ask`] and
//!    stops there. The kernel serialises the prompts, handles a host with no UI
//!    by refusing, and reports the answer back through
//!    [`ToolGate::record_answer`]. A gate therefore cannot accidentally implement
//!    "ask" as "allow", and cannot be the reason two dialogs race.
//!
//! So a gate may only *tighten*: it chooses between allow, ask and deny for a
//! call the guard has already permitted. It cannot loosen anything.

use thunder_agent_types::policy::{Caller, ToolEffect};
use thunder_agent_types::scope::Scope;
use serde::{Deserialize, Serialize};

/// One call, as presented to a gate.
///
/// Borrowed because the kernel already owns every one of these fields for the
/// duration of the call; cloning them per tool call would buy nothing.
#[derive(Debug)]
pub struct ToolGateRequest<'a> {
    /// The registered tool name — the name the model used.
    pub tool: &'a str,
    /// The arguments the tool will receive, already parsed.
    pub arguments: &'a serde_json::Value,
    /// What this call can change, from [`ToolEffect::of`] or the tool's own
    /// declaration.
    pub effect: ToolEffect,
    /// Where it lands, from the host's
    /// [`ToolScope`](thunder_agent_types::scope::ToolScope); [`Scope::Unknown`] when no
    /// scope is installed.
    pub scope: Scope,
    /// Who asked for it.
    pub caller: Caller,
    /// The tool call id, for correlating the decision with its trace.
    pub tool_call_id: &'a str,
}

/// What a gate says should happen to a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum GateVerdict {
    /// Run it.
    Allow,
    /// Run it because a standing rule the user already agreed to matches.
    ///
    /// Distinct from [`GateVerdict::Allow`] so the trace can show *why* the call
    /// went through unattended; the kernel treats the two identically.
    AllowByRule {
        /// A short human-readable label for the rule, e.g. `write_file src/`.
        rule: String,
    },
    /// Ask the user, with this prompt.
    Ask(ApprovalPrompt),
    /// Refuse it. The `reason` is what the model is told.
    Deny {
        /// Shown to the model as the tool result.
        reason: String,
    },
}

/// The question a gate wants asked.
///
/// Deliberately not a UI description: the kernel renders the dialog, so a gate
/// cannot make an approval look like an ordinary question, and cannot offer a
/// footgun the kernel does not know how to honour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    /// The one line the user reads first, e.g. `Allow write_file outside the
    /// workspace?`.
    pub title: String,
    /// What exactly will run, and where.
    pub detail: String,
    /// Whether this call can be narrowed to a replay-safe standing rule. When
    /// `false` the kernel offers only the one-shot answers, so a gate that
    /// cannot express the call safely does not have to guard against a blanket
    /// "always allow".
    pub allow_always: bool,
}

/// The user's answer to an [`ApprovalPrompt`], as handed back to the gate.
///
/// Only real answers appear here; a dialog that timed out or had no host UI never
/// produced one, and the kernel refuses in those cases on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum ApprovalAnswer {
    /// Run this call, once.
    AllowOnce,
    /// Run it, and remember a rule.
    AllowAlways,
    /// Refuse it.
    Deny,
    /// Refuse it, in the user's own words.
    DenyWithReason {
        /// The user's explanation, passed through to the model.
        reason: String,
    },
}

/// Decides what happens to a tool call, on the host's behalf.
///
/// See the [module docs](self) for what the kernel guarantees regardless of the
/// implementation. The kernel calls [`ToolGate::decide`] once per call, after the
/// guard, and never caches the answer.
pub trait ToolGate: Send + Sync {
    /// Decide one call.
    ///
    /// Deliberately synchronous and infallible: a decision is a pure function of
    /// the request and whatever state the gate keeps, and anything that needs to
    /// await or ask belongs to the kernel. Returning
    /// [`GateVerdict::Ask`] is how a gate hands the question over.
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict;

    /// Learn the user's answer to a prompt this gate asked for.
    ///
    /// A gate that offers `allow_always` records the narrowed rule here — and
    /// owns getting that narrowing right: a rule that is too broad pre-approves
    /// calls the user never saw, and a rule that re-derives its key from anything
    /// the model controls is replayable. The default does nothing, which is the
    /// right choice for a gate that never sets `allow_always`.
    fn record_answer(&self, _request: &ToolGateRequest<'_>, _answer: &ApprovalAnswer) {}

    /// A short label for traces and status bars, e.g. the active mode's name.
    ///
    /// The kernel never interprets it — it is stored on the trace and displayed.
    fn label(&self) -> &str {
        "gate"
    }
}

/// Options the kernel offers on an approval dialog, in display order.
///
/// These live here, not with a policy: the dialog is the kernel's
/// (see the [module docs](self)), so its vocabulary is too. A gate asks
/// *whether*; the kernel decides what the user is allowed to answer.
pub const ALLOW_ONCE: &str = "Allow once";
pub const ALLOW_ALWAYS: &str = "Always allow";
pub const DENY: &str = "Deny";
pub const DENY_WITH_REASON: &str = "Deny with reason";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verdicts_and_answers_ride_the_wire_as_tagged_objects() {
        assert_eq!(
            serde_json::to_value(GateVerdict::AllowByRule {
                rule: "read".into()
            })
            .unwrap(),
            json!({ "verdict": "allow_by_rule", "rule": "read" })
        );
        assert_eq!(
            serde_json::from_value::<GateVerdict>(json!({ "verdict": "allow" })).unwrap(),
            GateVerdict::Allow
        );
        assert_eq!(
            serde_json::from_value::<ApprovalAnswer>(
                json!({ "answer": "deny_with_reason", "reason": "not that file" })
            )
            .unwrap(),
            ApprovalAnswer::DenyWithReason {
                reason: "not that file".into()
            }
        );
    }

    #[test]
    fn a_request_carries_the_scope_and_the_effect_the_guard_left_behind() {
        let arguments = json!({ "path": "src/main.rs" });
        let request = ToolGateRequest {
            tool: "write_file",
            arguments: &arguments,
            effect: ToolEffect::Write,
            scope: Scope::InScope,
            caller: Caller::Model,
            tool_call_id: "call_1",
        };

        assert_eq!(request.effect, ToolEffect::Write);
        assert_eq!(request.scope, Scope::InScope);
        assert_eq!(request.tool_call_id, "call_1");
    }
}