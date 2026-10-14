//! OSC 7501 — what this run tells the *terminal* it is doing.
//!
//! The status plugin answers "what is the agent doing?" for a host that reads
//! thunder's event stream. This module answers the same question for a program
//! that only sees the bytes on a pty — a panel running this TUI inside a pane,
//! which is how `agent-resume-panel` hosts `cli:thunder`. OSC 7501 is the
//! Program Status Protocol such a panel scans for, so the pane lights up with no
//! socket, no handshake, and no cooperation from the panel.
//!
//! Three rules come from the consumer's parser and are not ours to negotiate:
//!
//! - **No `id=` key.** A record carrying one describes a *child* program and is
//!   not allowed to own the pane; the report is dropped.
//! - **`state` is a closed set** — `working | blocked | error | idle | done |
//!   clear`. An unknown state discards the whole report rather than guessing.
//! - **The alphabet is narrow**: keys are `[a-z]{1,16}`, values are
//!   `[A-Za-z0-9_.,+/=-]*`, and one sequence — `ESC ] 7501 ;` through `ESC \` —
//!   may not exceed 4096 bytes.
//!
//! The terminal is told **only when the reported status changes**. A sequence
//! per frame would be noise on the pty for a consumer that updates per report,
//! and the screen already carries the per-frame detail.
//!
//! The report starts at the first frame rather than at the first run: an `idle`
//! report is also what *names* the pane, because the consumer learns which
//! program occupies it from the report itself. A TUI that stayed silent until
//! work started would be an anonymous pane until then.

use thunder_agent_root::plugins::{AgentPhase, AgentStatusSnapshot};

/// The `app=` this TUI reports under.
///
/// It is also what a consumer keys the pane's identity off (`osc7501:thunder`),
/// so it has to stay stable and be a name that means *this program*, not this
/// run: a per-run value here would rename the pane on every restart.
pub const APP: &str = "thunder";

/// `ESC ] 7501 ;`
const OSC_7501: &str = "\x1b]7501;";
/// String Terminator, chosen over BEL because it cannot be eaten by a terminal
/// that treats the bell specially.
const ST: &str = "\x1b\\";

/// The report for a TUI that is no longer running: the pane outlives it.
const RESTING_BODY: &str = "app=thunder:state=idle";

/// What the terminal has been told, and what it still has to be told.
///
/// Owned by [`App`](crate::app::App) rather than by the runner so that the
/// "only on change" rule survives redraws and restarts of the draw loop: the
/// runner is handed one sequence to write and keeps no state of its own.
#[derive(Debug, Default)]
pub struct PaneStatus {
    /// The body of the last report this process queued or wrote. `None` until
    /// the first one, which is also how `leaving` knows whether this process
    /// ever spoke at all.
    last: Option<String>,
    /// A sequence waiting for the next frame to write it.
    pending: Option<String>,
}

impl PaneStatus {
    /// Fold a fresh snapshot into the next write, if it says anything new.
    pub fn observe(&mut self, snapshot: &AgentStatusSnapshot) {
        let body = body_for(snapshot);
        if self.last.as_deref() == Some(body.as_str()) {
            return;
        }
        self.pending = Some(frame(&body));
        self.last = Some(body);
    }

    /// The sequence the next frame must write, if the status moved.
    pub fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    /// Queue a resting report for a TUI that is exiting.
    ///
    /// A pane outlives the program inside it. Once a program has reported, the
    /// consumer stops reading that pane's screen entirely, so a TUI that exits
    /// mid-run would otherwise leave the pane saying `working` for as long as
    /// the pane lives — and the shell prompt that replaces it would never bring
    /// it back down.
    ///
    /// A status that has never reported anything stays silent, and a report that
    /// is already the resting one is not repeated: the first is what keeps a
    /// host that never spoke from claiming the pane, the second what keeps the
    /// exit quiet for a TUI that has been idle all along.
    pub fn leaving(&mut self) {
        if self.last.is_none() || self.last.as_deref() == Some(RESTING_BODY) {
            return;
        }
        self.pending = Some(frame(RESTING_BODY));
        self.last = Some(RESTING_BODY.to_string());
    }
}

/// The report body for a snapshot, e.g. `app=thunder:state=working:kind=bash`.
fn body_for(snapshot: &AgentStatusSnapshot) -> String {
    let mut body = format!("app={APP}:state={}", state_for(snapshot.phase));
    // The tool name is free text — model-authored for MCP tools — and the
    // value alphabet is not. A name that cannot be spelled legally costs the
    // `kind` pair, not the report: "working" is the part a pane needs.
    if let Some(kind) = snapshot.tool.as_deref().and_then(expressible) {
        body.push_str(":kind=");
        body.push_str(kind);
    }
    body
}

/// The state a phase reports.
///
/// Coarser than the phase on purpose: the protocol has one word for "busy", and
/// a consumer that wanted more would be reading a screen, which is the thing
/// this replaces.
fn state_for(phase: AgentPhase) -> &'static str {
    match phase {
        AgentPhase::Idle => "idle",
        AgentPhase::Thinking
        | AgentPhase::Streaming
        | AgentPhase::Tool
        | AgentPhase::Compacting => "working",
        // A wait is not a failure, but from the outside it is the same fact:
        // nothing moves until the user does something.
        AgentPhase::AwaitingUser | AgentPhase::Paused => "blocked",
        AgentPhase::Done => "done",
        AgentPhase::Error => "error",
    }
}

/// `name` if the protocol can carry it, `None` otherwise.
fn expressible(name: &str) -> Option<&str> {
    let name = name.trim();
    let legal = |c: char| c.is_ascii_alphanumeric() || "_.+,/=-".contains(c);
    (!name.is_empty() && name.chars().all(legal)).then_some(name)
}

/// Wrap a body in the protocol's framing.
fn frame(body: &str) -> String {
    format!("{OSC_7501}{body}{ST}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(phase: AgentPhase, tool: Option<&str>) -> AgentStatusSnapshot {
        AgentStatusSnapshot {
            session_id: "sess-1".to_string(),
            phase,
            tool: tool.map(str::to_string),
            turn: 3,
            detail: None,
            started_at_ms: 1_791_940_658_296,
            seq: 7,
        }
    }

    /// The consumer's parser, in miniature — the parts that decide whether a
    /// report is honoured *at all*. Mirrored here so a change that would only
    /// show up as a silently ignored pane fails a unit test instead.
    fn parse(sequence: &str) -> Vec<(String, String)> {
        assert!(sequence.starts_with("\x1b]7501;"), "{sequence:?}");
        assert!(sequence.ends_with("\x1b\\"), "{sequence:?}");
        assert!(sequence.len() <= 4_096);
        let body = &sequence["\x1b]7501;".len()..sequence.len() - "\x1b\\".len()];

        let mut pairs = Vec::new();
        for pair in body.split(':') {
            let (key, value) = pair.split_once('=').unwrap_or_else(|| panic!("{pair:?}"));
            assert!(
                (1..=16).contains(&key.len()) && key.chars().all(|c| c.is_ascii_lowercase()),
                "illegal key {key:?}"
            );
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.+,/=-".contains(c)),
                "illegal value {value:?}"
            );
            pairs.push((key.to_string(), value.to_string()));
        }
        // An `id` would make this a child record, which owns no pane.
        assert!(!pairs.iter().any(|(k, _)| k == "id"), "got {pairs:?}");
        pairs
    }

    fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_working_run_names_the_tool_it_is_running() {
        let mut status = PaneStatus::default();
        status.observe(&snapshot(AgentPhase::Tool, Some("bash")));
        let pairs = parse(&status.take_pending().expect("a sequence"));
        assert_eq!(value(&pairs, "app"), Some("thunder"));
        assert_eq!(value(&pairs, "state"), Some("working"));
        assert_eq!(value(&pairs, "kind"), Some("bash"));
    }

    #[test]
    fn every_phase_lands_on_a_state_the_protocol_knows() {
        // Mirrors the consumer's own table: these six are the whole vocabulary,
        // and a phase that mapped anywhere else would be dropped as a whole
        // report — a silently dead pane.
        for (phase, expected) in [
            (AgentPhase::Idle, "idle"),
            (AgentPhase::Thinking, "working"),
            (AgentPhase::Streaming, "working"),
            (AgentPhase::Tool, "working"),
            (AgentPhase::Compacting, "working"),
            (AgentPhase::AwaitingUser, "blocked"),
            (AgentPhase::Paused, "blocked"),
            (AgentPhase::Done, "done"),
            (AgentPhase::Error, "error"),
        ] {
            let body = body_for(&snapshot(phase, None));
            assert_eq!(value(&parse(&frame(&body)), "state"), Some(expected));
        }
    }

    #[test]
    fn a_tool_name_the_protocol_cannot_spell_costs_only_the_kind_pair() {
        let mut status = PaneStatus::default();
        // Real MCP names carry spaces, parentheses and colons; a colon would
        // even split the pair in two.
        status.observe(&snapshot(
            AgentPhase::Tool,
            Some("mcp__gh__create issue (x)"),
        ));
        let pairs = parse(&status.take_pending().expect("a sequence"));
        assert_eq!(value(&pairs, "state"), Some("working"));
        assert_eq!(value(&pairs, "kind"), None);
    }

    #[test]
    fn the_terminal_is_told_only_when_the_report_moves() {
        let mut status = PaneStatus::default();
        // Two snapshots of the same phase: the plugin publishes both (the turn
        // moved), the pane has nothing new to say.
        status.observe(&snapshot(AgentPhase::Tool, Some("bash")));
        assert!(status.take_pending().is_some());
        status.observe(&snapshot(AgentPhase::Tool, Some("bash")));
        assert!(status.take_pending().is_none());

        status.observe(&snapshot(AgentPhase::Streaming, None));
        let pairs = parse(&status.take_pending().expect("a sequence"));
        assert_eq!(value(&pairs, "state"), Some("working"));
        assert_eq!(value(&pairs, "kind"), None);
    }

    #[test]
    fn a_resting_tui_leaves_the_pane_idle() {
        let mut status = PaneStatus::default();
        status.observe(&snapshot(AgentPhase::Tool, Some("bash")));
        let _ = status.take_pending();

        status.leaving();
        let pairs = parse(&status.take_pending().expect("a sequence"));
        assert_eq!(value(&pairs, "state"), Some("idle"));
    }

    #[test]
    fn a_resting_tui_that_was_already_idle_says_nothing_and_a_silent_one_never_speaks() {
        let mut quiet = PaneStatus::default();
        quiet.leaving();
        assert!(quiet.take_pending().is_none(), "silence stays silence");

        let mut idle = PaneStatus::default();
        idle.observe(&snapshot(AgentPhase::Idle, None));
        let _ = idle.take_pending();
        idle.leaving();
        assert!(
            idle.take_pending().is_none(),
            "idle is already the resting report"
        );
    }
}
