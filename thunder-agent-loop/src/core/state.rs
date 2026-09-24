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
        let avg_tps = if total_dur_ms > 0 && self.total_completion_tokens > 0 {
            Some((self.total_completion_tokens as f64) / (total_dur_ms as f64 / 1000.0))
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
