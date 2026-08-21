use crate::types::message::ToolCall;
use crate::types::tool::ToolExecutionResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnStats {
    pub turn: usize,
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
    pub duration_ms: u64,
    pub tool_calls_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentStats {
    pub total_turns: usize,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub total_duration_ms: u64,
    pub total_tool_executions: usize,
    pub total_tool_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Done,
    MaxTurnsExceeded,
    BudgetExceeded,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStart {
        turn: usize,
        timestamp: u64,
    },
    TokenDelta {
        turn: usize,
        delta: String,
    },
    ToolCallChunk {
        turn: usize,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    ToolCallReady {
        turn: usize,
        tool_call: ToolCall,
    },
    ToolExecStart {
        turn: usize,
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolExecResult {
        turn: usize,
        tool_call_id: String,
        name: String,
        result: ToolExecutionResult,
    },
    TurnEnd {
        turn: usize,
        finish_reason: String,
        stats: TurnStats,
    },
    LoopComplete {
        finish_reason: FinishReason,
        final_content: Option<String>,
        stats: AgentStats,
    },
    Error {
        turn: Option<usize>,
        message: String,
        recoverable: bool,
    },
}
