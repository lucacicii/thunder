use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRecord {
    pub tool_name: String,
    pub arguments: String,
}

/// Default number of recent tool results the error-rate breaker looks at.
pub const DEFAULT_ERROR_WINDOW: usize = 12;
/// Default number of errors inside that window that trips the breaker.
pub const DEFAULT_MAX_WINDOW_ERRORS: usize = 8;

/// Circuit breaker and repetition detector preventing infinite tool-call loops.
#[derive(Debug, Clone)]
pub struct LoopGuard {
    recent_actions: VecDeque<ActionRecord>,
    max_history: usize,
    repetition_threshold: usize,
    hard_repetition_limit: usize,
    consecutive_errors: usize,
    max_consecutive_errors: usize,
    /// Recent tool outcome flags (`true` = error), newest last.
    recent_errors: VecDeque<bool>,
    error_window: usize,
    max_window_errors: usize,
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self::new(10, 3, 5, 5)
    }
}

impl LoopGuard {
    pub fn new(
        max_history: usize,
        repetition_threshold: usize,
        hard_repetition_limit: usize,
        max_consecutive_errors: usize,
    ) -> Self {
        Self {
            recent_actions: VecDeque::with_capacity(max_history),
            max_history,
            repetition_threshold,
            hard_repetition_limit,
            consecutive_errors: 0,
            max_consecutive_errors,
            recent_errors: VecDeque::with_capacity(DEFAULT_ERROR_WINDOW),
            error_window: DEFAULT_ERROR_WINDOW,
            max_window_errors: DEFAULT_MAX_WINDOW_ERRORS,
        }
    }

    pub fn record_and_check_repetition(
        &mut self,
        tool_name: &str,
        arguments: &str,
    ) -> Option<String> {
        let record = ActionRecord {
            tool_name: tool_name.to_string(),
            arguments: arguments.trim().to_string(),
        };

        if self.recent_actions.len() >= self.max_history {
            self.recent_actions.pop_front();
        }
        self.recent_actions.push_back(record.clone());

        if self.recent_actions.len() >= self.repetition_threshold {
            let recent_slice = self
                .recent_actions
                .iter()
                .rev()
                .take(self.repetition_threshold);
            let all_match = recent_slice
                .clone()
                .all(|r| r.tool_name == record.tool_name && r.arguments == record.arguments);

            if all_match {
                return Some(format!(
                    "[System Notice: You have executed the exact same tool '{}' with identical arguments {} times in a row without progress. Please verify the output, modify your strategy, or summarize your findings.]",
                    tool_name, self.repetition_threshold
                ));
            }
        }

        None
    }

    pub fn is_repetition_limit_exceeded(&self) -> bool {
        if self.recent_actions.len() >= self.hard_repetition_limit {
            if let Some(last) = self.recent_actions.back() {
                return self
                    .recent_actions
                    .iter()
                    .rev()
                    .take(self.hard_repetition_limit)
                    .all(|r| r.tool_name == last.tool_name && r.arguments == last.arguments);
            }
        }
        false
    }

    /// Override the error-rate breaker (window size and how many errors inside it trip it).
    pub fn with_error_window(mut self, window: usize, max_errors: usize) -> Self {
        self.error_window = window.max(1);
        self.max_window_errors = max_errors.max(1);
        self.recent_errors.clear();
        self
    }

    pub fn record_tool_result(&mut self, is_error: bool) {
        if is_error {
            self.consecutive_errors += 1;
        } else {
            self.consecutive_errors = 0;
        }

        self.recent_errors.push_back(is_error);
        while self.recent_errors.len() > self.error_window {
            self.recent_errors.pop_front();
        }
    }

    /// Errors within the sliding window, regardless of interspersed successes.
    pub fn window_errors(&self) -> usize {
        self.recent_errors.iter().filter(|e| **e).count()
    }

    pub fn is_circuit_broken(&self) -> bool {
        // Fast path: a clean streak of failures.
        if self.consecutive_errors >= self.max_consecutive_errors {
            return true;
        }
        // Slow path: `err, err, ok, err, err, ok …` never builds a streak, but it is
        // just as stuck. Judge the ratio instead of the run length.
        self.window_errors() >= self.max_window_errors
    }

    pub fn consecutive_errors(&self) -> usize {
        self.consecutive_errors
    }

    pub fn error_window(&self) -> usize {
        self.error_window
    }

    pub fn reset(&mut self) {
        self.recent_actions.clear();
        self.recent_errors.clear();
        self.consecutive_errors = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittering_errors_trip_the_window_breaker() {
        // `err, err, ok` repeated: the consecutive counter is cleared by every success,
        // so only the window can catch it.
        let mut g = LoopGuard::new(10, 3, 5, 5).with_error_window(12, 8);
        let pattern = [true, true, false];
        let mut tripped_after = None;
        for i in 0..24 {
            g.record_tool_result(pattern[i % 3]);
            assert!(
                g.consecutive_errors() <= 2,
                "the streak must never reach the fast-path threshold"
            );
            if g.is_circuit_broken() && tripped_after.is_none() {
                tripped_after = Some(i + 1);
            }
        }
        let tripped_after = tripped_after.expect("jittering errors must trip the breaker");
        assert_eq!(
            tripped_after, 11,
            "the 11th result brings the 8th error inside the 12-result window"
        );
    }

    #[test]
    fn occasional_errors_never_trip_the_window_breaker() {
        let mut g = LoopGuard::new(10, 3, 5, 5).with_error_window(12, 8);
        for i in 0..100 {
            // One failure every fourth call: 25% error rate, a healthy run.
            g.record_tool_result(i % 4 == 0);
            assert!(!g.is_circuit_broken(), "tripped at result {i}");
        }
        assert_eq!(g.window_errors(), 3);
    }

    #[test]
    fn a_single_success_does_not_clear_the_window() {
        let mut g = LoopGuard::new(10, 3, 5, 100).with_error_window(6, 4);
        g.record_tool_result(true);
        g.record_tool_result(true);
        g.record_tool_result(false);
        assert_eq!(
            g.window_errors(),
            2,
            "the window keeps the earlier failures"
        );
        g.record_tool_result(true);
        g.record_tool_result(true);
        assert!(g.is_circuit_broken(), "4 of the last 6 failed");
    }

    #[test]
    fn reset_clears_the_error_window() {
        let mut g = LoopGuard::new(10, 3, 5, 100).with_error_window(6, 4);
        for _ in 0..4 {
            g.record_tool_result(true);
        }
        assert!(g.is_circuit_broken());
        g.reset();
        assert!(!g.is_circuit_broken());
        assert_eq!(g.window_errors(), 0);
    }

    #[test]
    fn detects_soft_and_hard_repetition_and_circuit_breaks() {
        let mut g = LoopGuard::new(10, 3, 5, 3);
        assert!(g.record_and_check_repetition("core__bash", "{}").is_none());
        assert!(g.record_and_check_repetition("core__bash", "{}").is_none());
        assert!(g.record_and_check_repetition("core__bash", "{}").is_some());
        assert!(!g.is_repetition_limit_exceeded());
        let _ = g.record_and_check_repetition("core__bash", "{}");
        let _ = g.record_and_check_repetition("core__bash", "{}");
        assert!(g.is_repetition_limit_exceeded());

        g.record_tool_result(true);
        g.record_tool_result(true);
        assert!(!g.is_circuit_broken());
        g.record_tool_result(true);
        assert!(g.is_circuit_broken());
    }
}
