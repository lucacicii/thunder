use crate::types::event::{AgentStats, TurnStats};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStatus {
    Idle,
    Running,
    ExecutingTools,
    Completed,
    Failed,
    Aborted,
}

pub struct AgentStateTracker {
    status: LoopStatus,
    current_turn: usize,
    start_time: Instant,
    total_prompt_tokens: usize,
    total_completion_tokens: usize,
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
        self.total_tool_executions += stats.tool_calls_count;
        self.turn_history.push(stats);
    }

    pub fn record_tool_execution(&mut self, duration_ms: u64) {
        self.total_tool_time_ms += duration_ms;
    }

    pub fn get_stats(&self) -> AgentStats {
        AgentStats {
            total_turns: self.current_turn,
            total_prompt_tokens: self.total_prompt_tokens,
            total_completion_tokens: self.total_completion_tokens,
            total_duration_ms: self.start_time.elapsed().as_millis() as u64,
            total_tool_executions: self.total_tool_executions,
            total_tool_time_ms: self.total_tool_time_ms,
        }
    }

    pub fn turn_history(&self) -> &[TurnStats] {
        &self.turn_history
    }
}
