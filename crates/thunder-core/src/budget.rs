//! Run-level budget accounting for the ReAct agent loop.
//!
//! Two different guards exist on purpose and must not be conflated:
//!
//! * **Stall detection** ([`crate::progress`], [`crate::loop_guard`]) stops runs that
//!   keep moving without producing anything new.
//! * **Budget** (this module) bounds runs that *are* making progress. No stall
//!   heuristic can end those, so a wall-clock/token ceiling is not optional.
//!
//! Phase C only *observes* the budget: it records usage and emits a single
//! near-limit notice. Enforcement (wrap-up turn, `finish_reason`) arrives in
//! phase D.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Static limits for one user prompt, shared across every recovery attempt of that
/// prompt (an automatic retry must never reset or recharge the budget).
#[derive(Debug, Clone, PartialEq)]
pub struct LoopBudget {
    /// Hard backstop on model turns. `0` disables it.
    pub max_turns: usize,
    /// Hard backstop on wall-clock time. `None` disables it.
    pub wall_clock: Option<Duration>,
    /// Hard backstop on prompt+completion tokens. `0` disables it.
    pub max_total_tokens: usize,
    /// Maximum tool calls dispatched from a single assistant turn. `0` disables it.
    pub max_tool_calls_per_turn: usize,
    /// Fraction of any limit at which a single near-limit notice is emitted.
    pub warn_at: f32,
    /// How many times a stalled context may be reset (recovery level L1).
    pub max_context_resets: usize,
    /// How many times the whole run may be restarted (recovery level L2).
    pub max_restarts: usize,
}

impl Default for LoopBudget {
    fn default() -> Self {
        Self {
            // Phase C keeps the historical 16 so no behaviour changes before the
            // wrap-up turn exists; phase D raises this together with enforcement.
            max_turns: 16,
            wall_clock: Some(Duration::from_secs(1800)),
            max_total_tokens: 2_000_000,
            max_tool_calls_per_turn: 16,
            warn_at: 0.75,
            max_context_resets: 2,
            max_restarts: 0,
        }
    }
}

impl LoopBudget {
    /// A budget with every limit disabled, for tests and for `max_turns = 0` configs.
    pub fn unlimited() -> Self {
        Self {
            max_turns: 0,
            wall_clock: None,
            max_total_tokens: 0,
            max_tool_calls_per_turn: 0,
            warn_at: 0.75,
            max_context_resets: 0,
            max_restarts: 0,
        }
    }
}

/// Why a budget check fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BudgetLimit {
    Turns,
    WallClock,
    Tokens,
    ToolCallsPerTurn,
}

impl BudgetLimit {
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetLimit::Turns => "max_turns",
            BudgetLimit::WallClock => "wall_clock",
            BudgetLimit::Tokens => "max_total_tokens",
            BudgetLimit::ToolCallsPerTurn => "max_tool_calls_per_turn",
        }
    }
}

/// Outcome of a budget check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetUsage {
    /// Still inside every limit.
    Ok,
    /// Crossed the `warn_at` fraction of at least one limit (reported once per run).
    NearLimit(BudgetLimit),
    /// A hard limit is reached; phase D turns this into a wrap-up turn.
    Exhausted(BudgetLimit),
}

/// Live budget accounting for one prompt, including its recovery attempts.
#[derive(Debug, Clone)]
pub struct BudgetState {
    budget: LoopBudget,
    started: Instant,
    turns_used: usize,
    tokens_used: usize,
    near_limit_announced: BTreeSet<BudgetLimit>,
    resets_used: usize,
    restarts_used: usize,
}

impl BudgetState {
    pub fn new(budget: LoopBudget, started: Instant) -> Self {
        Self {
            budget,
            started,
            turns_used: 0,
            tokens_used: 0,
            near_limit_announced: BTreeSet::new(),
            resets_used: 0,
            restarts_used: 0,
        }
    }

    pub fn budget(&self) -> &LoopBudget {
        &self.budget
    }

    pub fn turns_used(&self) -> usize {
        self.turns_used
    }

    pub fn tokens_used(&self) -> usize {
        self.tokens_used
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn resets_used(&self) -> usize {
        self.resets_used
    }

    pub fn restarts_used(&self) -> usize {
        self.restarts_used
    }

    /// Count one completed model turn. Must also be called for a turn that only
    /// produced tool calls, otherwise the backstop can be outrun.
    pub fn record_turn(&mut self) -> usize {
        self.turns_used += 1;
        self.turns_used
    }

    /// Add the token usage reported by the provider for the last turn.
    pub fn record_tokens(&mut self, prompt_tokens: usize, completion_tokens: usize) {
        self.tokens_used += prompt_tokens + completion_tokens;
    }

    fn fraction_used(&self, limit: BudgetLimit) -> Option<f32> {
        match limit {
            BudgetLimit::Turns if self.budget.max_turns > 0 => {
                Some(self.turns_used as f32 / self.budget.max_turns as f32)
            }
            BudgetLimit::Tokens if self.budget.max_total_tokens > 0 => {
                Some(self.tokens_used as f32 / self.budget.max_total_tokens as f32)
            }
            BudgetLimit::WallClock => self
                .budget
                .wall_clock
                .map(|total| self.elapsed().as_secs_f32() / total.as_secs_f32().max(f32::EPSILON)),
            _ => None,
        }
    }

    fn is_exhausted(&self, limit: BudgetLimit) -> bool {
        match limit {
            BudgetLimit::Turns => {
                self.budget.max_turns > 0 && self.turns_used >= self.budget.max_turns
            }
            BudgetLimit::Tokens => {
                self.budget.max_total_tokens > 0 && self.tokens_used >= self.budget.max_total_tokens
            }
            BudgetLimit::WallClock => self
                .budget
                .wall_clock
                .map(|total| self.elapsed() >= total)
                .unwrap_or(false),
            BudgetLimit::ToolCallsPerTurn => false,
        }
    }

    /// Check every active limit. Hard limits win over the warning. Each limit
    /// produces at most one `NearLimit` report per run.
    pub fn check(&mut self) -> BudgetUsage {
        const LIMITS: [BudgetLimit; 3] = [
            BudgetLimit::Turns,
            BudgetLimit::WallClock,
            BudgetLimit::Tokens,
        ];

        for limit in LIMITS {
            if self.is_exhausted(limit) {
                return BudgetUsage::Exhausted(limit);
            }
        }

        // Report the limit closest to exhaustion so the notice names the real risk.
        let mut worst: Option<(BudgetLimit, f32)> = None;
        for limit in LIMITS {
            if let Some(fraction) = self.fraction_used(limit) {
                if worst.map(|(_, f)| fraction > f).unwrap_or(true) {
                    worst = Some((limit, fraction));
                }
            }
        }

        if let Some((limit, fraction)) = worst {
            if fraction >= self.budget.warn_at && self.near_limit_announced.insert(limit) {
                return BudgetUsage::NearLimit(limit);
            }
        }
        BudgetUsage::Ok
    }

    /// Whether one more stalled-context reset is allowed (recovery L1).
    pub fn allow_context_reset(&mut self) -> bool {
        if self.resets_used < self.budget.max_context_resets {
            self.resets_used += 1;
            true
        } else {
            false
        }
    }

    /// Whether one more full restart is allowed (recovery L2).
    ///
    /// A restart deliberately does **not** reset `turns_used`, `tokens_used` or
    /// `started`: an automatic retry that recharges the budget would be an
    /// unbounded loop wearing a different hat.
    pub fn allow_restart(&mut self) -> bool {
        if self.restarts_used < self.budget.max_restarts {
            self.restarts_used += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(max_turns: usize, tokens: usize, wall: Option<u64>) -> LoopBudget {
        LoopBudget {
            max_turns,
            max_total_tokens: tokens,
            wall_clock: wall.map(Duration::from_secs),
            warn_at: 0.75,
            ..LoopBudget::default()
        }
    }

    #[test]
    fn stays_ok_below_the_warning_fraction() {
        let mut state = BudgetState::new(budget(10, 0, None), Instant::now());
        state.record_turn();
        state.record_turn();
        assert_eq!(state.check(), BudgetUsage::Ok);
    }

    #[test]
    fn warns_once_when_crossing_the_fraction() {
        let mut state = BudgetState::new(budget(4, 0, None), Instant::now());
        for _ in 0..2 {
            state.record_turn();
        }
        assert_eq!(state.check(), BudgetUsage::Ok, "50% is quiet");
        state.record_turn(); // 75%
        assert_eq!(state.check(), BudgetUsage::NearLimit(BudgetLimit::Turns));
        assert_eq!(state.check(), BudgetUsage::Ok, "notice must not repeat");
    }

    #[test]
    fn reports_exhaustion_of_turns() {
        let mut state = BudgetState::new(budget(2, 0, None), Instant::now());
        state.record_turn();
        assert_eq!(state.check(), BudgetUsage::Ok);
        state.record_turn();
        assert_eq!(
            state.check(),
            BudgetUsage::Exhausted(BudgetLimit::Turns),
            "exhaustion outranks the near-limit notice"
        );
    }

    #[test]
    fn reports_exhaustion_of_tokens() {
        let mut state = BudgetState::new(budget(0, 1000, None), Instant::now());
        state.record_tokens(400, 100);
        assert_eq!(state.check(), BudgetUsage::Ok, "50% is quiet");
        state.record_tokens(300, 0); // 80%
        assert_eq!(state.check(), BudgetUsage::NearLimit(BudgetLimit::Tokens));
        state.record_tokens(300, 0);
        assert_eq!(state.check(), BudgetUsage::Exhausted(BudgetLimit::Tokens));
    }

    #[test]
    fn reports_exhaustion_of_wall_clock() {
        let mut state = BudgetState::new(
            budget(0, 0, Some(0)),
            Instant::now() - Duration::from_secs(1),
        );
        assert_eq!(
            state.check(),
            BudgetUsage::Exhausted(BudgetLimit::WallClock)
        );
    }

    #[test]
    fn unlimited_budget_never_fires() {
        let mut state = BudgetState::new(LoopBudget::unlimited(), Instant::now());
        for _ in 0..500 {
            state.record_turn();
            state.record_tokens(10_000, 10_000);
        }
        assert_eq!(state.check(), BudgetUsage::Ok);
    }

    #[test]
    fn recovery_attempts_are_capped_and_keep_usage() {
        let mut state = BudgetState::new(
            LoopBudget {
                max_context_resets: 1,
                max_restarts: 1,
                ..LoopBudget::default()
            },
            Instant::now(),
        );
        state.record_turn();
        state.record_tokens(100, 50);

        assert!(state.allow_context_reset());
        assert!(!state.allow_context_reset());
        assert!(state.allow_restart());
        assert!(!state.allow_restart());

        assert_eq!(state.turns_used(), 1, "a restart must not recharge turns");
        assert_eq!(state.tokens_used(), 150);
    }
}
