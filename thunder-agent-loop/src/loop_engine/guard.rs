use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct ActionRecord {
    pub tool_name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct LoopGuard {
    recent_actions: VecDeque<ActionRecord>,
    max_history: usize,
    repetition_threshold: usize,
    hard_repetition_limit: usize,
    consecutive_errors: usize,
    max_consecutive_errors: usize,
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
        }
    }

    /// Records a tool invocation and checks if a repetition pattern / loop degradation is detected
    pub fn record_and_check_repetition(&mut self, tool_name: &str, arguments: &str) -> Option<String> {
        let record = ActionRecord {
            tool_name: tool_name.to_string(),
            arguments: arguments.trim().to_string(),
        };

        if self.recent_actions.len() >= self.max_history {
            self.recent_actions.pop_front();
        }
        self.recent_actions.push_back(record.clone());

        // Check if the last `repetition_threshold` actions are identical
        if self.recent_actions.len() >= self.repetition_threshold {
            let recent_slice = self.recent_actions.iter().rev().take(self.repetition_threshold);
            let all_match = recent_slice.clone().all(|r| r.tool_name == record.tool_name && r.arguments == record.arguments);

            if all_match {
                return Some(format!(
                    "[System Notice: You have executed the exact same tool '{}' with identical arguments {} times in a row without progress. Please verify the output, modify your strategy, or summarize your findings.]",
                    tool_name, self.repetition_threshold
                ));
            }
        }

        None
    }

    /// Returns true if an identical tool call has been repeated >= hard_repetition_limit times
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

    /// Records tool execution success/failure to maintain circuit breaker
    pub fn record_tool_result(&mut self, is_error: bool) {
        if is_error {
            self.consecutive_errors += 1;
        } else {
            self.consecutive_errors = 0;
        }
    }

    /// Returns true if consecutive errors exceeded threshold
    pub fn is_circuit_broken(&self) -> bool {
        self.consecutive_errors >= self.max_consecutive_errors
    }

    pub fn consecutive_errors(&self) -> usize {
        self.consecutive_errors
    }

    pub fn reset(&mut self) {
        self.recent_actions.clear();
        self.consecutive_errors = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loop_guard_repetition_detection_and_hard_limit() {
        let mut guard = LoopGuard::new(10, 3, 5, 5);

        assert!(guard.record_and_check_repetition("bash", "ls -la").is_none());
        assert!(guard.record_and_check_repetition("bash", "ls -la").is_none());
        let warning = guard.record_and_check_repetition("bash", "ls -la");
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("executed the exact same tool"));
        assert!(!guard.is_repetition_limit_exceeded());

        guard.record_and_check_repetition("bash", "ls -la");
        guard.record_and_check_repetition("bash", "ls -la");
        assert!(guard.is_repetition_limit_exceeded());
    }

    #[test]
    fn test_loop_guard_circuit_breaker() {
        let mut guard = LoopGuard::new(5, 3, 5, 3);
        assert!(!guard.is_circuit_broken());

        guard.record_tool_result(true);
        guard.record_tool_result(true);
        assert!(!guard.is_circuit_broken());

        guard.record_tool_result(true);
        assert!(guard.is_circuit_broken());

        guard.record_tool_result(false);
        assert!(!guard.is_circuit_broken());
    }
}
