//! Progress and stall detection for the ReAct agent loop.
//!
//! The kernel is deliberately tool-agnostic: it never parses file names, shell
//! commands or output formats. What it does need — and cannot derive itself — is
//! whether a tool call *changed* anything. Plugins declare that once, as
//! [`ProgressKind`] on their [`ToolDescriptor`]; this module turns those
//! declarations plus the raw arguments/output into a single fingerprint, and
//! tracks whether the run is still producing anything new.
//!
//! Everything here is observation-only in phase B: it feeds `tracing` and
//! `AgentStats`, and never changes control flow yet.

use serde_json::Value;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use thunder_protocol::{ProgressKind, ToolDescriptor};

/// Default number of consecutive turns without a new mutation before the run is
/// considered stalled. Only meaningful once the run has mutated something at all.
pub const DEFAULT_MUTATION_FREE_TURNS: usize = 8;

/// Default number of consecutive turns without any new observation digest.
pub const DEFAULT_NO_NOVEL_TURNS: usize = 6;

/// The single progress fingerprint used across the whole kernel.
///
/// `digest`s only live in memory for the duration of one run, so a
/// non-cryptographic `DefaultHasher` is sufficient and keeps the crate free of a
/// hashing dependency.
///
/// The variant-specific inputs encode each declaration's contract:
/// - [`ProgressKind::Mutating`] hashes the arguments only. Plugins promise that
///   identical arguments imply an identical world state, so rewriting a file with
///   the same content yields the same digest and counts as no progress.
/// - [`ProgressKind::Observing`] also hashes the output: the same read returning
///   the same text is the same information.
/// - [`ProgressKind::Neutral`] hashes the output only. Different commands that
///   print the same thing did not teach the agent anything new.
pub fn fingerprint(kind: ProgressKind, tool: &str, args: &str, output: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Domain separation: keeps a mutating digest from ever colliding with an
    // observing digest computed over the same tool.
    std::mem::discriminant(&kind).hash(&mut hasher);
    tool.hash(&mut hasher);
    match kind {
        ProgressKind::Mutating => args.hash(&mut hasher),
        ProgressKind::Observing => {
            args.hash(&mut hasher);
            output.hash(&mut hasher);
        }
        ProgressKind::Neutral => output.hash(&mut hasher),
    }
    hasher.finish()
}

/// Resolve the declared [`ProgressKind`] for an exposed tool name.
///
/// Unknown names (a tool that vanished from the snapshot mid-turn) fall back to
/// [`ProgressKind::Neutral`], i.e. output-only detection.
pub fn kind_of<'a>(
    descriptors: impl IntoIterator<Item = &'a ToolDescriptor>,
    tool_name: &str,
) -> ProgressKind {
    descriptors
        .into_iter()
        .find(|d| d.name == tool_name)
        .map(|d| d.progress)
        .unwrap_or(ProgressKind::Neutral)
}

/// Serialize tool arguments into the canonical string used for fingerprinting.
pub fn canonical_args(args: &Value) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
}

/// Why a run is considered stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallSignal {
    /// The run has mutated state before, but nothing new for N turns.
    MutationFree(usize),
    /// Nothing new was observed for N turns.
    NoNovelObservation(usize),
}

/// Thresholds for [`ProgressTracker`], exposed so config can tune them per workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallConfig {
    pub mutation_free_turns: usize,
    pub no_novel_turns: usize,
}

impl Default for StallConfig {
    fn default() -> Self {
        Self {
            mutation_free_turns: DEFAULT_MUTATION_FREE_TURNS,
            no_novel_turns: DEFAULT_NO_NOVEL_TURNS,
        }
    }
}

/// Tracks whether a run is still producing new information.
#[derive(Debug, Clone)]
pub struct ProgressTracker {
    /// Every mutation digest seen in this run (drives the mutation-free streak).
    mutations: HashSet<u64>,
    /// Every tool-result digest seen in this run, mutations included: a write is
    /// also new information, so it has to reset the no-novelty streak or a purely
    /// productive run would be reported as stalled.
    results: HashSet<u64>,
    mutation_free_streak: usize,
    no_novel_output_streak: usize,
    /// Highest value each streak reached in this run. Diagnostics only: the stall
    /// thresholds are tuned from these peaks, so they survive `reset()`.
    peak_mutation_free_streak: usize,
    peak_no_novel_streak: usize,
    turn_had_mutation: bool,
    turn_had_novel_output: bool,
    mutation_free_turns: usize,
    no_novel_turns: usize,
    total_mutations: usize,
    total_novel_observations: usize,
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::from_config(StallConfig::default())
    }
}

impl ProgressTracker {
    pub fn from_config(cfg: StallConfig) -> Self {
        Self::new(cfg.mutation_free_turns, cfg.no_novel_turns)
    }

    pub fn new(mutation_free_turns: usize, no_novel_turns: usize) -> Self {
        Self {
            mutations: HashSet::new(),
            results: HashSet::new(),
            mutation_free_streak: 0,
            no_novel_output_streak: 0,
            peak_mutation_free_streak: 0,
            peak_no_novel_streak: 0,
            turn_had_mutation: false,
            turn_had_novel_output: false,
            mutation_free_turns,
            no_novel_turns,
            total_mutations: 0,
            total_novel_observations: 0,
        }
    }

    /// Record one executed tool call. `output` is the text handed to the model.
    ///
    /// Returns `true` when the call produced a fingerprint that had not been seen
    /// in this run, i.e. when it counted as progress of its kind.
    pub fn record(&mut self, kind: ProgressKind, tool: &str, args: &str, output: &str) -> bool {
        let digest = fingerprint(kind, tool, args, output);
        let kind_is_new = match kind {
            ProgressKind::Mutating => {
                let novel = self.mutations.insert(digest);
                if novel {
                    self.total_mutations += 1;
                    self.turn_had_mutation = true;
                }
                novel
            }
            ProgressKind::Observing | ProgressKind::Neutral => true,
        };
        // A novel mutation is novel information too: it must clear the no-novelty
        // streak, otherwise a run that only writes new content looks stalled.
        let novel_result = self.results.insert(digest);
        if novel_result {
            self.total_novel_observations += 1;
            self.turn_had_novel_output = true;
        }
        kind_is_new
    }

    /// Close out a turn and report whether the run looks stalled.
    ///
    /// A run that never mutated anything (read-only investigation, Q&A) can never
    /// be judged mutation-stalled: stalling means "it started changing things and
    /// then stopped", not "it is exploring".
    pub fn on_turn_end(&mut self) -> Option<StallSignal> {
        // A mutation-free turn only counts against the run when it was also
        // informationally empty. Verification phases are the reason: a live run edited
        // one file and then spent several turns running tests, and every one of those
        // turns produced new output — counting them towards "stopped mutating" would
        // have cut a healthy run short.
        if self.turn_had_mutation || self.turn_had_novel_output {
            self.mutation_free_streak = 0;
        } else if !self.mutations.is_empty() {
            self.mutation_free_streak += 1;
        } else {
            self.mutation_free_streak = 0;
        }

        if self.turn_had_novel_output {
            self.no_novel_output_streak = 0;
        } else {
            self.no_novel_output_streak += 1;
        }

        self.turn_had_mutation = false;
        self.turn_had_novel_output = false;

        self.peak_mutation_free_streak = self
            .peak_mutation_free_streak
            .max(self.mutation_free_streak);
        self.peak_no_novel_streak = self.peak_no_novel_streak.max(self.no_novel_output_streak);

        if self.mutation_free_streak >= self.mutation_free_turns {
            return Some(StallSignal::MutationFree(self.mutation_free_streak));
        }
        if self.no_novel_output_streak >= self.no_novel_turns {
            return Some(StallSignal::NoNovelObservation(self.no_novel_output_streak));
        }
        None
    }

    pub fn total_mutations(&self) -> usize {
        self.total_mutations
    }

    pub fn total_novel_observations(&self) -> usize {
        self.total_novel_observations
    }

    pub fn mutation_free_streak(&self) -> usize {
        self.mutation_free_streak
    }

    pub fn no_novel_output_streak(&self) -> usize {
        self.no_novel_output_streak
    }

    /// Highest `mutation_free_streak` reached in this run (survives `reset()`).
    pub fn peak_mutation_free_streak(&self) -> usize {
        self.peak_mutation_free_streak
    }

    /// Highest `no_novel_output_streak` reached in this run (survives `reset()`).
    ///
    /// Compare this against `stall_no_novel_turns`: a run that never exceeded 2 is well
    /// clear of a threshold of 6, while one that reached 5 means the threshold is tight.
    pub fn peak_no_novel_streak(&self) -> usize {
        self.peak_no_novel_streak
    }

    /// Clear the run state, keeping the peaks: recovery resets detection, not the
    /// calibration data collected so far.
    pub fn reset(&mut self) {
        let (peak_mutation, peak_novel) =
            (self.peak_mutation_free_streak, self.peak_no_novel_streak);
        *self = Self::new(self.mutation_free_turns, self.no_novel_turns);
        self.peak_mutation_free_streak = peak_mutation;
        self.peak_no_novel_streak = peak_novel;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn desc(name: &str, progress: ProgressKind) -> ToolDescriptor {
        ToolDescriptor::new(name, "d", json!({}), format!("core://{name}")).with_progress(progress)
    }

    #[test]
    fn mutating_fingerprint_ignores_output() {
        let a = fingerprint(ProgressKind::Mutating, "write_file", "{\"p\":1}", "ok");
        let b = fingerprint(ProgressKind::Mutating, "write_file", "{\"p\":1}", "written");
        let c = fingerprint(ProgressKind::Mutating, "write_file", "{\"p\":2}", "ok");
        assert_eq!(a, b, "same args must mean the same world state");
        assert_ne!(a, c, "different args must be a different world state");
    }

    #[test]
    fn observing_fingerprint_includes_output() {
        let a = fingerprint(ProgressKind::Observing, "read_file", "{\"p\":1}", "v1");
        let b = fingerprint(ProgressKind::Observing, "read_file", "{\"p\":1}", "v2");
        assert_ne!(a, b);
    }

    #[test]
    fn neutral_fingerprint_ignores_args() {
        let a = fingerprint(ProgressKind::Neutral, "bash", "{\"c\":\"ls\"}", "same");
        let b = fingerprint(ProgressKind::Neutral, "bash", "{\"c\":\"ls -la\"}", "same");
        assert_eq!(
            a, b,
            "same output is the same information regardless of command"
        );
    }

    #[test]
    fn kinds_are_domain_separated() {
        let mutating = fingerprint(ProgressKind::Mutating, "t", "x", "y");
        let observing = fingerprint(ProgressKind::Observing, "t", "x", "y");
        assert_ne!(mutating, observing);
    }

    #[test]
    fn kind_lookup_falls_back_to_neutral() {
        let descriptors = [desc("core__write_file", ProgressKind::Mutating)];
        assert_eq!(
            kind_of(descriptors.iter(), "core__write_file"),
            ProgressKind::Mutating
        );
        assert_eq!(
            kind_of(descriptors.iter(), "mcp__ghost__tool"),
            ProgressKind::Neutral
        );
    }

    #[test]
    fn read_only_runs_are_never_mutation_stalled() {
        let mut tracker = ProgressTracker::new(3, 100);
        for turn in 0..20 {
            tracker.record(
                ProgressKind::Observing,
                "read_file",
                "{}",
                &format!("distinct output {turn}"),
            );
            assert_eq!(tracker.on_turn_end(), None, "investigation is not a stall");
        }
        assert_eq!(tracker.total_mutations(), 0);
        assert_eq!(tracker.mutation_free_streak(), 0);
    }

    #[test]
    fn a_verification_phase_is_not_a_stall() {
        // Edit once, then keep learning new things without editing again: this is a
        // healthy run (the live-run false positive that motivated the novelty check).
        let mut tracker = ProgressTracker::new(3, 100);
        tracker.record(ProgressKind::Mutating, "write_file", "{\"v\":1}", "ok");
        assert_eq!(tracker.on_turn_end(), None);

        for turn in 0..10 {
            tracker.record(
                ProgressKind::Neutral,
                "bash",
                &format!("{{\"c\":\"test {turn}\"}}"),
                &format!("a different test result {turn}"),
            );
            assert_eq!(
                tracker.on_turn_end(),
                None,
                "verification turns that produce new output are not a stall"
            );
        }
        assert_eq!(tracker.mutation_free_streak(), 0);
    }

    #[test]
    fn mutations_then_silence_triggers_mutation_free() {
        // Only reachable with a stricter-than-default no-novelty threshold: a mutation-
        // free turn has to be informationally empty too, so this rule is a backstop for
        // setups that raised `no_novel_turns`.
        let mut tracker = ProgressTracker::new(2, 100);
        tracker.record(ProgressKind::Mutating, "write_file", "{\"v\":1}", "ok");
        assert_eq!(tracker.on_turn_end(), None);

        // First identical read is still new information; the next two are not.
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(tracker.on_turn_end(), None);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(tracker.on_turn_end(), None);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(tracker.on_turn_end(), Some(StallSignal::MutationFree(2)));
    }

    #[test]
    fn rewriting_the_same_content_is_not_progress() {
        let mut tracker = ProgressTracker::new(2, 100);
        assert!(tracker.record(ProgressKind::Mutating, "write_file", "{\"v\":1}", "ok"));
        assert_eq!(tracker.on_turn_end(), None);
        assert!(
            !tracker.record(ProgressKind::Mutating, "write_file", "{\"v\":1}", "ok"),
            "identical rewrite must not count as progress"
        );
        assert_eq!(tracker.on_turn_end(), None);
        assert_eq!(tracker.mutation_free_streak(), 1);
    }

    #[test]
    fn peaks_record_the_worst_streaks_and_survive_reset() {
        // Thresholds far above what the run reaches: nothing stalls, but the peaks
        // still record how close it came — that is the calibration signal.
        let mut tracker = ProgressTracker::new(50, 50);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        tracker.on_turn_end(); // novel
        for _ in 0..4 {
            tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
            tracker.on_turn_end();
        }
        assert_eq!(tracker.no_novel_output_streak(), 4);
        assert_eq!(tracker.peak_no_novel_streak(), 4);

        // Break the streak, then reset the way recovery does.
        tracker.record(ProgressKind::Observing, "read_file", "{}", "fresh");
        tracker.on_turn_end();
        tracker.reset();
        assert_eq!(tracker.no_novel_output_streak(), 0, "detection restarts");
        assert_eq!(
            tracker.peak_no_novel_streak(),
            4,
            "calibration data must survive a recovery reset"
        );
    }

    #[test]
    fn no_novel_output_streak_triggers_without_mutations() {
        let mut tracker = ProgressTracker::new(100, 2);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(tracker.on_turn_end(), None, "first look is novel");

        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(
            tracker.on_turn_end(),
            None,
            "one fruitless turn is not enough"
        );

        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(
            tracker.on_turn_end(),
            Some(StallSignal::NoNovelObservation(2))
        );
    }

    #[test]
    fn a_novel_write_clears_the_no_novelty_streak() {
        let mut tracker = ProgressTracker::new(100, 2);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        tracker.on_turn_end();
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        assert_eq!(tracker.on_turn_end(), None);
        assert_eq!(tracker.no_novel_output_streak(), 1);

        // Writing new content is progress even though nothing was *read*: without
        // this, a productive write-only run would be reported as stalled.
        assert!(tracker.record(ProgressKind::Mutating, "write_file", "{\"v\":9}", "ok"));
        assert_eq!(tracker.on_turn_end(), None);
        assert_eq!(tracker.no_novel_output_streak(), 0);
        assert_eq!(tracker.mutation_free_streak(), 0);
    }

    #[test]
    fn novel_output_resets_the_streak() {
        let mut tracker = ProgressTracker::new(100, 2);
        tracker.record(ProgressKind::Observing, "read_file", "{}", "same");
        tracker.on_turn_end();
        tracker.record(ProgressKind::Observing, "read_file", "{}", "fresh");
        assert_eq!(tracker.on_turn_end(), None);
        assert_eq!(tracker.no_novel_output_streak(), 0);
    }
}
