//! Live execution-status plugin (`StatusPlugin`).
//!
//! Answers one question — *what is the agent doing right now?* — as a single
//! authoritative snapshot per session, so no host has to re-derive it from the
//! raw event stream. `thunder-tui` grew its own `AgentStatus` for exactly this
//! and it never left that process; this plugin is the same vocabulary, published
//! on the wire every host already reads.
//!
//! The snapshot rides the core event stream as
//! `AgentEvent::Custom { kind: "agent_status", payload }` — the same shape
//! [`MetricsPlugin`](super::MetricsPlugin) uses for `metrics_update` — because
//! the kernel deliberately does not interpret custom events, so a host that does
//! not know this kind ignores it.
//!
//! Two phases the event stream cannot express are supplied by the host, which is
//! the only party that knows them:
//!
//! - `awaiting_user` — a permission dialog or a question is on screen. The gate
//!   blocks inside `ToolExecutor::execute_all`, so *no further event arrives*
//!   while the user is being waited on; [`StatusPlugin::note_awaiting`] is what
//!   makes the wait visible.
//! - `paused` — the run is parked on a pause gate.
//!
//! A held phase is a *view* over the run's underlying phase, not a replacement:
//! answering a dialog raised for a tool call restores `tool` rather than
//! resetting to `thinking`, because that is still where the run is.

use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_agent_loop::types::event::{AgentEvent, FinishReason, ObservedEvent};
use tokio::sync::watch;

/// Custom event `kind` carrying an [`AgentStatusSnapshot`].
pub const AGENT_STATUS_EVENT_KIND: &str = "agent_status";

/// What a run is doing, in roughly the order a run moves through it.
///
/// Deliberately coarser than the token-level event stream, and disjoint from
/// [`MetricsSnapshot`](super::MetricsSnapshot): metrics say *how much*, this says
/// *what*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    /// No run, or a run that has not produced its first turn yet.
    Idle,
    /// A request is in flight and nothing has streamed back yet.
    Thinking,
    /// Answer or reasoning tokens are arriving.
    Streaming,
    /// One or more tool calls are executing. `tool` names the most recent one.
    Tool,
    /// Blocked on the user: a permission dialog or a question.
    AwaitingUser,
    /// Parked on the run's pause gate. Needs the user to resume.
    Paused,
    /// History is being compacted; the model is not being called.
    Compacting,
    /// The run finished. Sticky until the next run starts.
    Done,
    /// The run stopped with a failure to look at. Sticky like `done`.
    Error,
}

impl AgentPhase {
    /// Whether this phase means the run is over.
    ///
    /// Note this is not the same question as "is the run still publishing":
    /// a *cancelled* run settles on `idle`, which is also the resting phase
    /// between runs and therefore deliberately not terminal.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }
}

/// One run's execution status, as published to hosts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusSnapshot {
    pub session_id: String,
    pub phase: AgentPhase,
    /// The tool being executed. Present only while [`AgentPhase::Tool`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// 1-based turn within the run; 0 before the first turn starts.
    pub turn: usize,
    /// Free text for the current phase — an error message, a compaction delta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Wall-clock start of the run, so a consumer can show elapsed time without
    /// its own clock start (a panel that attaches late has no other way to know).
    pub started_at_ms: u64,
    /// Monotonic within the run, bumped on every change. A consumer drops a
    /// frame whose `seq` is not greater than the last it saw.
    pub seq: u64,
}

/// Which host-reported wait is on top of a run's underlying phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    AwaitingUser,
    Paused,
}

impl Hold {
    const ALL: [Hold; 2] = [Hold::AwaitingUser, Hold::Paused];

    fn index(self) -> usize {
        match self {
            Self::AwaitingUser => 0,
            Self::Paused => 1,
        }
    }

    fn phase(self) -> AgentPhase {
        match self {
            Self::AwaitingUser => AgentPhase::AwaitingUser,
            Self::Paused => AgentPhase::Paused,
        }
    }
}

/// Live status of one session's current run.
#[derive(Debug)]
struct SessionStatus {
    session_id: String,
    /// What the run is doing, ignoring any host-reported wait.
    base: AgentPhase,
    /// When each host-reported wait was entered, indexed by [`Hold::index`];
    /// `None` means that wait is not in effect.
    ///
    /// Tracked per wait rather than in one slot because the two are independent
    /// and neither should erase the other: a run can be parked on the pause gate
    /// *while* a question is on screen, and answering the question must not
    /// silently clear the pause.
    holds: [Option<u64>; 2],
    /// Bumped whenever a wait is entered, so "newest" is well defined when both
    /// are in effect.
    hold_clock: u64,
    /// Whether the run is over. Distinct from `base.is_terminal()` because a
    /// cancelled run settles on `idle`.
    ///
    /// Starts false: a session we have never seen is treated as live, so a host
    /// that only ever feeds us events (or reports a wait) is not silently
    /// ignored. `LoopComplete` and a fatal error latch it.
    settled: bool,
    tool: Option<String>,
    /// Tool calls announced by `tool_exec_start` and not yet answered, so a
    /// parallel batch keeps the tool phase until its last call lands.
    running_tools: HashSet<String>,
    turn: usize,
    detail: Option<String>,
    started_at_ms: u64,
    /// Bumped on every change, host-reported holds included.
    seq: u64,
}

impl SessionStatus {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            base: AgentPhase::Idle,
            holds: [None; 2],
            hold_clock: 0,
            settled: false,
            tool: None,
            running_tools: HashSet::new(),
            turn: 0,
            detail: None,
            started_at_ms: now_ms(),
            seq: 0,
        }
    }

    /// Reset for a new run. The run's wall clock restarts here, not on the first
    /// event: a panel that attaches late still gets the true start.
    fn begin_run(&mut self) -> bool {
        let previous = self.snapshot();
        self.base = AgentPhase::Idle;
        self.holds = [None; 2];
        self.settled = false;
        self.tool = None;
        self.running_tools.clear();
        self.turn = 0;
        self.detail = None;
        self.started_at_ms = now_ms();
        self.bump_if_changed(previous)
    }

    /// The phase a consumer should display: the newest wait, when one is in
    /// effect.
    fn phase(&self) -> AgentPhase {
        self.holds
            .iter()
            .enumerate()
            .filter_map(|(index, entered)| entered.map(|at| (at, index)))
            .max()
            .map(|(_, index)| Hold::ALL[index].phase())
            .unwrap_or(self.base)
    }

    fn snapshot(&self) -> AgentStatusSnapshot {
        let phase = self.phase();
        AgentStatusSnapshot {
            session_id: self.session_id.clone(),
            phase,
            tool: if phase == AgentPhase::Tool {
                self.tool.clone()
            } else {
                None
            },
            turn: self.turn,
            detail: self.detail.clone(),
            started_at_ms: self.started_at_ms,
            seq: self.seq,
        }
    }

    fn hold(&mut self, hold: Hold) -> bool {
        if self.settled || self.holds[hold.index()].is_some() {
            return false;
        }
        let previous = self.snapshot();
        self.hold_clock += 1;
        self.holds[hold.index()] = Some(self.hold_clock);
        self.bump_if_changed(previous)
    }

    fn release(&mut self, hold: Hold) -> bool {
        if self.holds[hold.index()].is_none() {
            return false;
        }
        let previous = self.snapshot();
        self.holds[hold.index()] = None;
        self.bump_if_changed(previous)
    }

    /// Adopt `next` as the underlying phase. Waits stay in effect: a token that
    /// arrives while a question is on screen does not answer it.
    fn set_base(&mut self, next: AgentPhase) {
        self.base = next;
        if next != AgentPhase::Tool {
            self.tool = None;
        }
    }

    fn bump_if_changed(&mut self, previous: AgentStatusSnapshot) -> bool {
        let mut normalised = previous;
        normalised.seq = self.seq;
        if self.snapshot() == normalised {
            return false;
        }
        self.seq += 1;
        true
    }

    /// Fold one kernel event into the phase. Returns true when the published
    /// snapshot changed.
    fn apply_event(&mut self, event: &AgentEvent) -> bool {
        // A settled run is not resurrected by the engine's unwind events: a
        // cancelled run has its unanswered tool calls answered *after*
        // `LoopComplete`, and those must not restart the status.
        if self.settled && !matches!(event, AgentEvent::LoopComplete { .. }) {
            return false;
        }
        let previous = self.snapshot();
        match event {
            AgentEvent::TurnStart { turn, .. } => {
                self.turn = *turn;
                self.set_base(AgentPhase::Thinking);
            }
            AgentEvent::ReasoningDelta { .. } | AgentEvent::TokenDelta { .. } => {
                self.set_base(AgentPhase::Streaming);
            }
            AgentEvent::ToolExecStart {
                tool_call_id, name, ..
            } => {
                self.running_tools.insert(tool_call_id.clone());
                self.tool = Some(name.clone());
                self.set_base(AgentPhase::Tool);
            }
            AgentEvent::ToolExecResult { tool_call_id, .. } => {
                self.running_tools.remove(tool_call_id);
                if !self.running_tools.is_empty() {
                    // A parallel batch keeps the tool phase until its last call
                    // lands; only then does the loop go back to the model.
                    return false;
                }
                self.set_base(AgentPhase::Thinking);
            }
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                ..
            } => {
                self.set_base(AgentPhase::Compacting);
                // Set after the base, so this arm's change test sees the detail
                // it is about to publish rather than the previous one.
                self.detail = Some(format!("{tokens_before} → {tokens_after} tokens"));
            }
            AgentEvent::Error {
                message,
                recoverable,
                ..
            } => {
                if *recoverable {
                    return false;
                }
                // The engine emits a non-recoverable error on its way out, so
                // the run is over even before `LoopComplete` lands.
                self.settled = true;
                self.holds = [None; 2];
                self.running_tools.clear();
                self.set_base(AgentPhase::Error);
                self.detail = Some(message.clone());
            }
            AgentEvent::LoopComplete { finish_reason, .. } => {
                self.settled = true;
                self.holds = [None; 2];
                self.running_tools.clear();
                // A deliberate stop is an outcome, not a failure — the same call
                // the TUI makes when it lands a cancelled run on `Idle` instead
                // of leaving an error banner behind.
                let next = match finish_reason {
                    FinishReason::Done | FinishReason::MaxTurnsExceeded => AgentPhase::Done,
                    FinishReason::BudgetExceeded | FinishReason::Error => AgentPhase::Error,
                    FinishReason::Cancelled => AgentPhase::Idle,
                };
                self.set_base(next);
                if next == AgentPhase::Done {
                    self.detail = None;
                }
            }
            // Everything else — token-level tool-call chunks, turn ends, traces,
            // steering chatter, other plugins' custom events — moves no phase.
            _ => return false,
        }
        self.bump_if_changed(previous)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// First-class plugin publishing "what is the agent doing right now" per session.
///
/// Registered by [`StandardHostBuilder`](super::StandardHostBuilder) and forced
/// active for every run, so all hosts — `thunder-tui`, `thunder-daemon`, and
/// Rust embedders — see the same status.
#[derive(Clone)]
pub struct StatusPlugin {
    manifest: PluginManifest,
    sessions: Arc<RwLock<HashMap<String, SessionStatus>>>,
    /// Wakes a host that is publishing this plugin's snapshots onto its own wire.
    ///
    /// Host-reported holds have no event to ride on — that is the whole point of
    /// them — so a publisher that only looked at the event stream would never
    /// see `awaiting_user`. The value is a revision, not a payload: a woken
    /// publisher looks up its own session's snapshot and decides.
    changes: watch::Sender<u64>,
    change_rx: watch::Receiver<u64>,
}

impl Default for StatusPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for StatusPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusPlugin")
            .field("id", &self.manifest.id)
            .finish()
    }
}

impl StatusPlugin {
    /// The id this plugin registers and is selected under.
    pub const ID: &'static str = "agent_status";

    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            Self::ID,
            "Thunder Agent Execution Status",
            "Publishes what the agent is doing right now — phase, current tool, turn — as one authoritative snapshot per session.",
            "0.1.0",
        )
        .with_capability(PluginCapability::Custom("status".to_string()))
        .with_triggers(TriggerSpec::always());
        let (changes, change_rx) = watch::channel(0);

        Self {
            manifest,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            changes,
            change_rx,
        }
    }

    /// Read the latest status for `session_id`.
    pub fn snapshot(&self, session_id: &str) -> Option<AgentStatusSnapshot> {
        self.sessions
            .read()
            .get(session_id)
            .map(SessionStatus::snapshot)
    }

    /// A receiver that fires whenever *any* session's status changes.
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.change_rx.clone()
    }

    /// Report that this session is (or is no longer) waiting on the user.
    ///
    /// Called by the host that raised the dialog, which is the only party that
    /// knows: the gate blocks inside tool execution and emits nothing.
    pub fn note_awaiting(&self, session_id: &str, waiting: bool) -> bool {
        self.note_hold(session_id, Hold::AwaitingUser, waiting)
    }

    /// Report that this session's run is (or is no longer) parked on its pause gate.
    pub fn note_paused(&self, session_id: &str, paused: bool) -> bool {
        self.note_hold(session_id, Hold::Paused, paused)
    }

    /// Start (or restart) the run on this session without a plugin host.
    ///
    /// [`ThunderPlugin::on_init`] does this for every run that goes through a
    /// `ThunderRoot`. A host that drives the agent loop directly has no plugin
    /// host to call it, and without the reset a second run in the same session
    /// would inherit the first one's settled phase — the new run's events would
    /// be dropped as an unwind of a run that already ended.
    pub fn begin_run(&self, session_id: &str) -> bool {
        self.mutate(session_id, SessionStatus::begin_run)
    }

    /// Record an event directly, for a caller holding the plugin without a run.
    pub fn record_session_event(&self, session_id: &str, event: &AgentEvent) -> bool {
        self.mutate(session_id, |status| status.apply_event(event))
    }

    fn note_hold(&self, session_id: &str, hold: Hold, on: bool) -> bool {
        self.mutate(session_id, |status| {
            if on {
                status.hold(hold)
            } else {
                status.release(hold)
            }
        })
    }

    fn mutate(&self, session_id: &str, edit: impl FnOnce(&mut SessionStatus) -> bool) -> bool {
        let (changed, revision) = {
            let mut guard = self.sessions.write();
            let status = guard
                .entry(session_id.to_string())
                .or_insert_with(|| SessionStatus::new(session_id));
            let changed = edit(status);
            (changed, status.seq)
        };
        if changed {
            self.changes.send_replace(revision);
        }
        changed
    }
}

#[async_trait]
impl ThunderPlugin for StatusPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // A new run on this session restarts the clock and clears the previous
        // run's settled phase.
        self.mutate(&ctx.session_id, SessionStatus::begin_run);
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, ctx: &PluginContext) {
        self.record_session_event(&ctx.session_id, &event.event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use thunder_agent_loop::types::event::{AgentStats, TurnStats};
    use thunder_agent_loop::types::tool::ToolExecutionResult;

    fn turn_start(turn: usize) -> AgentEvent {
        AgentEvent::TurnStart {
            turn,
            timestamp: 0,
            estimated_prompt_tokens: 0,
        }
    }

    fn tool_call(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolExecStart {
            turn: 1,
            tool_call_id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        }
    }

    fn tool_result(id: &str) -> AgentEvent {
        AgentEvent::ToolExecResult {
            turn: 1,
            tool_call_id: id.to_string(),
            name: "bash".to_string(),
            result: ToolExecutionResult::success("ok".to_string(), Duration::ZERO),
        }
    }

    fn loop_complete(reason: FinishReason) -> AgentEvent {
        AgentEvent::LoopComplete {
            finish_reason: reason,
            final_content: None,
            stats: AgentStats::default(),
        }
    }

    fn phase_of(plugin: &StatusPlugin, session: &str) -> AgentPhase {
        plugin.snapshot(session).expect("snapshot").phase
    }

    fn seq_of(plugin: &StatusPlugin, session: &str) -> u64 {
        plugin.snapshot(session).expect("snapshot").seq
    }

    #[test]
    fn a_run_walks_from_thinking_to_the_current_tool_and_back() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";

        plugin.record_session_event(session, &turn_start(1));
        assert_eq!(phase_of(&plugin, session), AgentPhase::Thinking);

        plugin.record_session_event(
            session,
            &AgentEvent::TokenDelta {
                turn: 1,
                delta: "hi".into(),
            },
        );
        assert_eq!(phase_of(&plugin, session), AgentPhase::Streaming);

        plugin.record_session_event(session, &tool_call("call_1", "bash"));
        let snapshot = plugin.snapshot(session).expect("snapshot");
        assert_eq!(snapshot.phase, AgentPhase::Tool);
        assert_eq!(snapshot.tool.as_deref(), Some("bash"));
        assert_eq!(snapshot.turn, 1);

        plugin.record_session_event(session, &tool_result("call_1"));
        let snapshot = plugin.snapshot(session).expect("snapshot");
        assert_eq!(snapshot.phase, AgentPhase::Thinking);
        assert!(snapshot.tool.is_none());
    }

    #[test]
    fn a_parallel_tool_batch_keeps_the_tool_phase_until_its_last_call_lands() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";

        plugin.record_session_event(session, &tool_call("call_1", "bash"));
        plugin.record_session_event(session, &tool_call("call_2", "write_file"));
        plugin.record_session_event(session, &tool_result("call_1"));

        assert_eq!(phase_of(&plugin, session), AgentPhase::Tool);
        assert_eq!(
            plugin.snapshot(session).expect("snapshot").tool.as_deref(),
            Some("write_file")
        );

        plugin.record_session_event(session, &tool_result("call_2"));
        assert_eq!(phase_of(&plugin, session), AgentPhase::Thinking);
    }

    #[test]
    fn a_host_reported_wait_is_a_view_over_the_underlying_phase() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &tool_call("call_1", "bash"));

        // The gate blocks inside the tool call, so the run is still *on* bash
        // while the user reads the dialog.
        assert!(plugin.note_awaiting(session, true));
        assert_eq!(phase_of(&plugin, session), AgentPhase::AwaitingUser);

        assert!(plugin.note_awaiting(session, false));
        assert_eq!(phase_of(&plugin, session), AgentPhase::Tool);
        assert_eq!(
            plugin.snapshot(session).expect("snapshot").tool.as_deref(),
            Some("bash")
        );
    }

    #[test]
    fn a_pause_outranks_a_pending_question_and_both_release_cleanly() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &turn_start(1));

        plugin.note_awaiting(session, true);
        plugin.note_paused(session, true);
        assert_eq!(phase_of(&plugin, session), AgentPhase::Paused);

        // Releasing the pause falls back to the question still on screen.
        plugin.note_paused(session, false);
        assert_eq!(phase_of(&plugin, session), AgentPhase::AwaitingUser);

        plugin.note_awaiting(session, false);
        assert_eq!(phase_of(&plugin, session), AgentPhase::Thinking);
    }

    #[test]
    fn the_seq_only_moves_when_the_published_snapshot_changes() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &turn_start(1));
        plugin.record_session_event(
            session,
            &AgentEvent::TokenDelta {
                turn: 1,
                delta: "first".into(),
            },
        );
        let settled = seq_of(&plugin, session);

        // The remaining deltas are the bulk of the stream and move no phase.
        for _ in 0..5 {
            plugin.record_session_event(
                session,
                &AgentEvent::TokenDelta {
                    turn: 1,
                    delta: "x".into(),
                },
            );
        }
        assert_eq!(seq_of(&plugin, session), settled);

        plugin.note_awaiting(session, true);
        let after_hold = seq_of(&plugin, session);
        assert!(after_hold > settled);

        // A repeat of the same hold is not a change either.
        plugin.note_awaiting(session, true);
        assert_eq!(seq_of(&plugin, session), after_hold);
    }

    #[test]
    fn a_settled_run_is_not_resurrected_by_the_engines_unwind_events() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &tool_call("call_1", "bash"));
        plugin.record_session_event(session, &loop_complete(FinishReason::Cancelled));
        assert_eq!(phase_of(&plugin, session), AgentPhase::Idle);

        // A cancelled run has its unanswered calls answered as it unwinds, and
        // those events arrive after `LoopComplete`.
        plugin.record_session_event(session, &tool_result("call_1"));
        assert_eq!(phase_of(&plugin, session), AgentPhase::Idle);
        assert!(plugin.snapshot(session).expect("snapshot").tool.is_none());
    }

    #[test]
    fn a_clean_finish_is_done_and_a_failure_is_error() {
        let plugin = StatusPlugin::new();
        let done = "sess_done";
        plugin.record_session_event(done, &loop_complete(FinishReason::Done));
        assert_eq!(phase_of(&plugin, done), AgentPhase::Done);
        assert!(AgentPhase::Done.is_terminal());

        let failed = "sess_failed";
        plugin.record_session_event(
            failed,
            &AgentEvent::Error {
                turn: Some(1),
                message: "boom".into(),
                recoverable: false,
            },
        );
        assert_eq!(phase_of(&plugin, failed), AgentPhase::Error);

        // Only a new run leaves a settled phase.
        plugin.record_session_event(failed, &turn_start(1));
        assert_eq!(phase_of(&plugin, failed), AgentPhase::Error);
    }

    #[test]
    fn a_recoverable_error_does_not_take_the_run_terminal() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &turn_start(1));
        plugin.record_session_event(
            session,
            &AgentEvent::Error {
                turn: Some(1),
                message: "transient".into(),
                recoverable: true,
            },
        );
        assert_eq!(phase_of(&plugin, session), AgentPhase::Thinking);
    }

    #[test]
    fn an_error_detail_reaches_the_snapshot_and_a_new_run_clears_it() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(
            session,
            &AgentEvent::Error {
                turn: Some(1),
                message: "model refused".into(),
                recoverable: false,
            },
        );
        let snapshot = plugin.snapshot(session).expect("snapshot");
        assert_eq!(snapshot.detail.as_deref(), Some("model refused"));

        assert!(plugin.mutate(session, SessionStatus::begin_run));
        let snapshot = plugin.snapshot(session).expect("snapshot");
        assert_eq!(snapshot.phase, AgentPhase::Idle);
        assert!(snapshot.detail.is_none());
        assert_eq!(snapshot.turn, 0);
    }

    #[test]
    fn compaction_reports_its_token_delta() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(
            session,
            &AgentEvent::ContextCompacted {
                turn: Some(1),
                tokens_before: 120_000,
                tokens_after: 8_000,
            },
        );
        let snapshot = plugin.snapshot(session).expect("snapshot");
        assert_eq!(snapshot.phase, AgentPhase::Compacting);
        assert_eq!(snapshot.detail.as_deref(), Some("120000 → 8000 tokens"));
    }

    #[test]
    fn the_snapshot_serializes_as_the_documented_wire_shape() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &tool_call("call_1", "bash"));
        let snapshot = plugin.snapshot(session).expect("snapshot");

        let wire = serde_json::to_value(&snapshot).expect("serialize");
        assert_eq!(wire["phase"], "tool");
        assert_eq!(wire["tool"], "bash");
        assert_eq!(wire["session_id"], "sess_1");
        assert!(wire["started_at_ms"].as_u64().is_some());
        // `detail` is absent rather than null, so a consumer can test presence.
        assert!(wire.get("detail").is_none());
    }

    #[test]
    fn a_change_wakes_a_subscribed_host() {
        let plugin = StatusPlugin::new();
        let rx = plugin.subscribe_changes();
        assert!(!rx.has_changed().expect("alive"));

        plugin.note_awaiting("sess_1", true);
        assert!(rx.has_changed().expect("alive"));
    }

    #[test]
    fn a_turn_end_moves_no_phase() {
        let plugin = StatusPlugin::new();
        let session = "sess_1";
        plugin.record_session_event(session, &tool_call("call_1", "bash"));
        let settled = seq_of(&plugin, session);

        plugin.record_session_event(
            session,
            &AgentEvent::TurnEnd {
                turn: 1,
                finish_reason: "tool_calls".into(),
                stats: TurnStats::default(),
            },
        );
        assert_eq!(seq_of(&plugin, session), settled);
        assert_eq!(phase_of(&plugin, session), AgentPhase::Tool);
    }

    #[test]
    fn the_manifest_makes_the_plugin_available_to_every_run() {
        let plugin = StatusPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, "agent_status");
        assert!(manifest.triggers.auto_always);
    }
}
