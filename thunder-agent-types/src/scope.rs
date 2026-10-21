//! Where a tool call lands, as classified by the host.
//!
//! The kernel cannot know what "inside the project" means — that is the host's
//! fact, not the kernel's ([`crate::scope::ToolScope`]). What the kernel
//! owns is what to do with the answer, and the answer is deliberately narrow: a
//! scope is a *location*, never a verdict.
//!
//! The asymmetry is the point. A scope may only ever be used to *relax* a call
//! the gate would otherwise ask about (an in-scope write under `acceptEdits`
//! runs without a prompt); it may never be used to turn a call the gate would
//! allow into a refusal. Every new path heuristic therefore costs the user at
//! most one extra prompt, never a silent failure.
//!
//! [`Scope::Critical`] is the one exception, and it is deliberately narrow: it is
//! the host telling the kernel "this target is not a question for the user, it is
//! off limits". A host that overuses it rebuilds the problem this design exists to
//! remove, so it belongs only on targets where asking would be *wrong* rather than
//! merely inconvenient — a repository's `.git` directory, a credential store.

use serde::{Deserialize, Serialize};

/// Where a call's effect lands, relative to the host's working area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Inside the host's declared working area.
    InScope,
    /// Outside it. The call is not thereby refused — it just does not earn the
    /// automatic allowance that an in-scope target does.
    OutOfScope,
    /// Off limits: the kernel refuses this call unconditionally, in every mode,
    /// without asking.
    ///
    /// The host names it (a repository's `.git` directory, a credential store),
    /// the kernel enforces it — see the module docs for why this is the one
    /// classification that is not merely a location. Excepted from it, always:
    /// the guard's own dangerous-command list, which the host cannot loosen.
    Critical,
    /// The host cannot tell — a shell command, a path built from a variable, a
    /// tool it does not recognise.
    ///
    /// This is the default, and the safe answer: an unknown target earns no
    /// automatic allowance, so the call is asked about rather than run.
    #[default]
    Unknown,
}

/// Classifies where a call will land, for hosts that know.
///
/// Implemented by the host (a capability pack, typically), installed on the loop
/// like the gate, and consulted once per call between the guard and the gate.
/// With no scope installed, every call is [`Scope::Unknown`].
///
/// Implementations must be cheap and total: this runs on the tool-call path, and
/// an inability to decide is answered by returning [`Scope::Unknown`] rather than
/// by failing.
pub trait ToolScope: Send + Sync {
    /// Where `tool`'s call, with these already-parsed `arguments`, will land.
    ///
    /// `effect` is passed because the answer depends on it: writing `/etc/hosts`
    /// and reading it are not the same act. It is the tool's own declared effect,
    /// so a host never has to infer direction from a tool's name.
    ///
    /// Deliberately not `async`: this is a pure classification of data the kernel
    /// already holds, and a scope that needs to ask the user something is a gate
    /// in the wrong place.
    fn scope_of(
        &self,
        tool: &str,
        effect: crate::policy::ToolEffect,
        arguments: &serde_json::Value,
    ) -> Scope;
}

/// A [`ToolScope`] that knows nothing, for tests and for hosts that have no
/// working area to speak of (the headless runtime among them).
#[derive(Debug, Clone, Copy, Default)]
pub struct UnknownScope;

impl ToolScope for UnknownScope {
    fn scope_of(
        &self,
        _tool: &str,
        _effect: crate::policy::ToolEffect,
        _arguments: &serde_json::Value,
    ) -> Scope {
        Scope::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_is_the_default_answer() {
        assert_eq!(Scope::default(), Scope::Unknown);
        assert_eq!(
            UnknownScope.scope_of(
                "write_file",
                crate::policy::ToolEffect::Write,
                &json!({ "path": "/etc/hosts" })
            ),
            Scope::Unknown
        );
    }

    #[test]
    fn scope_rides_the_wire_as_snake_case() {
        assert_eq!(
            serde_json::to_value(Scope::InScope).unwrap(),
            json!("in_scope")
        );
        assert_eq!(
            serde_json::from_value::<Scope>(json!("out_of_scope")).unwrap(),
            Scope::OutOfScope
        );
    }
}