use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRecord {
    pub tool_name: String,
    pub arguments: String,
}

/// Circuit breaker and repetition detector preventing infinite tool-call loops.
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

    pub fn record_tool_result(&mut self, is_error: bool) {
        if is_error {
            self.consecutive_errors += 1;
        } else {
            self.consecutive_errors = 0;
        }
    }

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
