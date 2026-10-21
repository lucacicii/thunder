use crate::types::event::{AgentStats, TurnStats};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LoopStatus {
    Idle = 0,
    Running = 1,
    ExecutingTools = 2,
    Completed = 3,
    Failed = 4,
    Aborted = 5,
}

impl LoopStatus {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Running,
            2 => Self::ExecutingTools,
            3 => Self::Completed,
            4 => Self::Failed,
            5 => Self::Aborted,
            _ => Self::Idle,
        }
    }
}

pub struct AgentStateTracker {
    status: LoopStatus,
    current_turn: usize,
    start_time: Instant,
    total_prompt_tokens: usize,
    total_completion_tokens: usize,
    total_cached_tokens: usize,
    total_reasoning_tokens: usize,
    total_tool_executions: usize,
    total_tool_time_ms: u64,
    total_generation_duration_ms: u64,
    turn_history: Vec<TurnStats>,
}

impl Default for AgentStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentStateTracker {
    pub fn new() -> Self {
        Self {
            status: LoopStatus::Idle,
            current_turn: 0,
            start_time: Instant::now(),
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            total_cached_tokens: 0,
            total_reasoning_tokens: 0,
            total_tool_executions: 0,
            total_tool_time_ms: 0,
            total_generation_duration_ms: 0,
            turn_history: Vec::new(),
        }
    }

    pub fn status(&self) -> LoopStatus {
        self.status
    }

    pub fn set_status(&mut self, status: LoopStatus) {
        self.status = status;
    }

    pub fn current_turn(&self) -> usize {
        self.current_turn
    }

    pub fn next_turn(&mut self) -> usize {
        self.current_turn += 1;
        self.current_turn
    }

    pub fn record_turn_stats(&mut self, stats: TurnStats) {
        if let Some(pt) = stats.prompt_tokens {
            self.total_prompt_tokens += pt;
        }
        if let Some(ct) = stats.completion_tokens {
            self.total_completion_tokens += ct;
            if ct > 0 {
                let gen_ms = match stats.tokens_per_second {
                    Some(tps) if tps > 0.0 => ((ct as f64 / tps) * 1000.0).round() as u64,
                    _ => stats.duration_ms,
                };
                self.total_generation_duration_ms += gen_ms.max(1);
            }
        }
        if let Some(cached) = stats.cached_tokens {
            self.total_cached_tokens += cached;
        }
        if let Some(reasoning) = stats.reasoning_tokens {
            self.total_reasoning_tokens += reasoning;
        }
        self.total_tool_executions += stats.tool_calls_count;
        self.turn_history.push(stats);
    }

    pub fn record_tool_execution(&mut self, duration_ms: u64) {
        self.total_tool_time_ms += duration_ms;
    }

    pub fn get_stats(&self) -> AgentStats {
        let total_dur_ms = self.start_time.elapsed().as_millis() as u64;
        let gen_dur_ms = if self.total_generation_duration_ms > 0 {
            self.total_generation_duration_ms
        } else {
            total_dur_ms.saturating_sub(self.total_tool_time_ms)
        };
        let avg_tps = if gen_dur_ms > 0 && self.total_completion_tokens > 0 {
            Some((self.total_completion_tokens as f64) / (gen_dur_ms as f64 / 1000.0))
        } else {
            None
        };

        AgentStats {
            total_turns: self.current_turn,
            total_prompt_tokens: self.total_prompt_tokens,
            total_completion_tokens: self.total_completion_tokens,
            total_cached_tokens: self.total_cached_tokens,
            total_reasoning_tokens: self.total_reasoning_tokens,
            total_duration_ms: total_dur_ms,
            total_tool_executions: self.total_tool_executions,
            total_tool_time_ms: self.total_tool_time_ms,
            avg_tokens_per_second: avg_tps,
        }
    }

    pub fn turn_history(&self) -> &[TurnStats] {
        &self.turn_history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avg_tps_uses_turn_generation_durations_and_excludes_tool_wait() {
        let mut tracker = AgentStateTracker::new();
        tracker.next_turn();
        // Turn 1 generated 160 tokens at 80 tok/s (2.0s decode time), even if
        // total turn wall time was 5.0s due to 3.0s TTFT.
        tracker.record_turn_stats(TurnStats {
            turn: 1,
            prompt_tokens: Some(4000),
            completion_tokens: Some(160),
            cached_tokens: Some(0),
            cache_write_tokens: None,
            reasoning_tokens: Some(60),
            duration_ms: 5000,
            tool_calls_count: 1,
            tokens_per_second: Some(80.0),
        });
        // Tool takes 20s to execute
        tracker.record_tool_execution(20_000);

        tracker.next_turn();
        // Turn 2 generated 240 tokens at 80 tok/s (3.0s decode time)
        tracker.record_turn_stats(TurnStats {
            turn: 2,
            prompt_tokens: Some(4500),
            completion_tokens: Some(240),
            cached_tokens: Some(4000),
            cache_write_tokens: None,
            reasoning_tokens: Some(40),
            duration_ms: 4000,
            tool_calls_count: 0,
            tokens_per_second: Some(80.0),
        });

        let stats = tracker.get_stats();
        assert_eq!(stats.total_completion_tokens, 400);
        let avg = stats.avg_tokens_per_second.expect("avg tps");
        assert!((avg - 80.0).abs() < 0.1, "expected ~80 tok/s, got {avg}");
    }
}