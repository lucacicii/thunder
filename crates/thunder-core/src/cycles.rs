//! Cyclic-repetition detection for the agent loop.
//!
//! [`crate::loop_guard`] catches "the same call N times in a row". This module catches
//! the other common degenerate shape: `A-B-A-B-A-B`. It is deliberately *not* a
//! "did the same thing twice" heuristic — a cycle is only condemned when it also
//! produced **no new fingerprint** inside the window, so legitimate alternating work
//! (edit a, edit b, edit a with new content, …) never trips it.

use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use thunder_protocol::ProgressKind;

/// One signature per tool call: the *action*, not its result.
///
/// `output` is intentionally excluded — the identity of an action is its tool plus
/// its arguments. Two identical reads of the same file are the same action, and a
/// `bash` with different arguments is a different action.
pub fn fingerprint_action(kind: ProgressKind, tool: &str, args: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&kind).hash(&mut hasher);
    tool.hash(&mut hasher);
    args.hash(&mut hasher);
    hasher.finish()
}

/// A detected cycle: `period` distinct actions repeated `cycles` times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleHit {
    pub period: usize,
    pub cycles: usize,
}

/// What to do about a detected cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleAction {
    /// No verdict.
    None,
    /// Inject a "change your approach" notice into the tool result.
    SoftWarn(CycleHit),
    /// Abort the run and let the caller wrap up.
    HardAbort(CycleHit),
}

/// Configuration of the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleConfig {
    /// Smallest cycle length considered. `1` is excluded on purpose: "the same call
    /// repeatedly" is [`crate::loop_guard`]'s job, and a period of 1 matches everything.
    pub min_period: usize,
    pub max_period: usize,
    /// How many actions are remembered for cycle detection.
    pub window: usize,
    /// Cycles needed before a soft warning.
    pub soft_cycles: usize,
    /// Cycles needed before a hard abort (which also requires zero novelty).
    pub hard_cycles: usize,
}

impl Default for CycleConfig {
    fn default() -> Self {
        Self {
            min_period: 2,
            max_period: 4,
            window: 24,
            soft_cycles: 3,
            hard_cycles: 4,
        }
    }
}

/// Tail-anchored lag-`p` periodicity test.
///
/// Returns the strongest hit: the run of actions ending at the newest entry that
/// repeats every `p` steps. Anchoring at the tail means a single action that breaks
/// the pattern resets the verdict immediately.
pub fn detect_cycle(seq: &[(u64, bool)], cfg: CycleConfig) -> Option<CycleHit> {
    let n = seq.len();
    let mut best: Option<CycleHit> = None;

    for period in cfg.min_period..=cfg.max_period {
        if n < period + 1 {
            continue;
        }
        let mut pairs = 0usize;
        for i in 0..(n - period) {
            if seq[n - 1 - i].0 == seq[n - 1 - period - i].0 {
                pairs += 1;
            } else {
                break;
            }
        }
        // Two full repetitions beyond the first occurrence (`A-B-A-B-A-B`).
        if pairs >= 2 * period {
            let cycles = 1 + pairs / period;
            if best.map(|b| cycles > b.cycles).unwrap_or(true) {
                best = Some(CycleHit { period, cycles });
            }
        }
    }
    best
}

/// Sliding window of recent action signatures plus their novelty flags.
#[derive(Debug, Clone)]
pub struct CycleDetector {
    cfg: CycleConfig,
    seq: VecDeque<(u64, bool)>,
    seen: HashSet<u64>,
    /// Suppresses repeated soft warnings for the same cycle.
    warned_until_len: usize,
    soft_warnings: usize,
}

impl Default for CycleDetector {
    fn default() -> Self {
        Self::new(CycleConfig::default())
    }
}

impl CycleDetector {
    pub fn new(cfg: CycleConfig) -> Self {
        let window = cfg.window.max(cfg.max_period * (cfg.hard_cycles + 1));
        Self {
            cfg,
            seq: VecDeque::with_capacity(window),
            seen: HashSet::new(),
            warned_until_len: 0,
            soft_warnings: 0,
        }
    }

    /// Record one tool call (by action signature) and judge it.
    pub fn record(&mut self, action: u64) -> CycleAction {
        let novel = self.seen.insert(action);
        self.seq.push_back((action, novel));
        while self.seq.len() > self.cfg.window {
            self.seq.pop_front();
        }

        let Some(hit) = detect_cycle(self.seq.make_contiguous(), self.cfg) else {
            return CycleAction::None;
        };

        if hit.cycles >= self.cfg.hard_cycles {
            return CycleAction::HardAbort(hit);
        }
        if hit.cycles >= self.cfg.soft_cycles && self.seq.len() >= self.warned_until_len {
            self.warned_until_len = self.seq.len() + hit.period * 2;
            self.soft_warnings += 1;
            return CycleAction::SoftWarn(hit);
        }
        CycleAction::None
    }

    pub fn soft_warnings(&self) -> usize {
        self.soft_warnings
    }

    pub fn reset(&mut self) {
        let cfg = self.cfg;
        *self = Self::new(cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(s: &str) -> u64 {
        fingerprint_action(ProgressKind::Observing, s, "{}")
    }

    fn feed(det: &mut CycleDetector, actions: &[&str]) -> Vec<CycleAction> {
        actions.iter().map(|a| det.record(sig(a))).collect()
    }

    #[test]
    fn abab_is_not_a_cycle_yet() {
        let mut det = CycleDetector::default();
        let out = feed(&mut det, &["a", "b", "a", "b"]);
        assert!(out.iter().all(|a| *a == CycleAction::None), "{out:?}");
    }

    #[test]
    fn ababab_warns_softly() {
        let mut det = CycleDetector::default();
        let out = feed(&mut det, &["a", "b", "a", "b", "a", "b"]);
        assert_eq!(
            out.last(),
            Some(&CycleAction::SoftWarn(CycleHit {
                period: 2,
                cycles: 3
            }))
        );
    }

    #[test]
    fn abababab_aborts_because_nothing_is_new() {
        let mut det = CycleDetector::default();
        let out = feed(&mut det, &["a", "b", "a", "b", "a", "b", "a", "b"]);
        assert_eq!(
            out.last(),
            Some(&CycleAction::HardAbort(CycleHit {
                period: 2,
                cycles: 4
            }))
        );
    }

    #[test]
    fn period_three_is_detected() {
        let mut det = CycleDetector::new(CycleConfig {
            soft_cycles: 3,
            ..CycleConfig::default()
        });
        let out = feed(&mut det, &["a", "b", "c", "a", "b", "c", "a", "b", "c"]);
        assert_eq!(
            out.last(),
            Some(&CycleAction::SoftWarn(CycleHit {
                period: 3,
                cycles: 3
            }))
        );
    }

    #[test]
    fn a_novel_action_breaks_the_cycle() {
        let mut det = CycleDetector::default();
        feed(&mut det, &["a", "b", "a", "b", "a", "b"]);
        // Same shape but a brand-new action: not a cycle any more.
        let out = feed(&mut det, &["c"]);
        assert_eq!(out, vec![CycleAction::None]);
    }

    #[test]
    fn alternating_writes_with_new_content_are_progress() {
        let mut det = CycleDetector::default();
        // Same two tools, different arguments every time: genuinely moving forward.
        let mut actions = Vec::new();
        for i in 0..8 {
            actions.push(format!("write-file-a-v{i}"));
            actions.push(format!("write-file-b-v{i}"));
        }
        let refs: Vec<&str> = actions.iter().map(String::as_str).collect();
        let out = feed(&mut det, &refs);
        assert!(
            out.iter().all(|a| *a == CycleAction::None),
            "evolving content must never be flagged: {out:?}"
        );
    }

    #[test]
    fn a_longer_cycle_needs_more_actions_before_aborting() {
        // Period 3 aborts on the 12th action (4 cycles), not the 6th.
        let mut det = CycleDetector::default();
        let out = feed(
            &mut det,
            &["a", "b", "c", "a", "b", "c", "a", "b", "c", "a", "b", "c"],
        );
        assert_eq!(
            out.last(),
            Some(&CycleAction::HardAbort(CycleHit {
                period: 3,
                cycles: 4
            }))
        );
    }

    #[test]
    fn soft_warning_is_rate_limited() {
        let mut det = CycleDetector::default();
        let out = feed(
            &mut det,
            &["a", "b", "a", "b", "a", "b", "a", "b", "a", "b", "a", "b"],
        );
        let warns = out
            .iter()
            .filter(|a| matches!(a, CycleAction::SoftWarn(_)))
            .count();
        assert_eq!(warns, 1, "one warning per cycle window: {out:?}");
    }

    #[test]
    fn action_signature_ignores_nothing_it_should_not() {
        let a = fingerprint_action(ProgressKind::Mutating, "write_file", "{\"v\":1}");
        let b = fingerprint_action(ProgressKind::Mutating, "write_file", "{\"v\":2}");
        let c = fingerprint_action(ProgressKind::Observing, "write_file", "{\"v\":1}");
        assert_ne!(a, b);
        assert_ne!(a, c, "kinds are domain separated");
        assert_eq!(
            a,
            fingerprint_action(ProgressKind::Mutating, "write_file", "{\"v\":1}")
        );
    }
}
