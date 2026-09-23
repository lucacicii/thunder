use crate::tools::scratchpad::ScratchpadConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Recommended autonomous agent prompt guiding the LLM to call tools when needed and conclude when done.
pub const DEFAULT_AUTONOMOUS_SYSTEM_PROMPT: &str = "\
You are an autonomous AI agent capable of using tools to solve complex tasks.
Guidelines:
1. If you need additional information or must execute an operation, call the appropriate tool(s).
2. When you have sufficient information to fulfill the user's request, do NOT invoke any further tools; formulate and output your final answer directly to conclude the execution loop.";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruningStrategy {
    TruncateToolResults,
    SlidingWindow,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPruningConfig {
    pub max_context_tokens: usize,
    /// Number of recent turns to preserve with full fidelity during pruning / compaction (default: 3)
    pub preserve_last_turns: usize,
    pub pin_system_prompt: bool,
    pub strategy: PruningStrategy,
}

impl Default for ContextPruningConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            preserve_last_turns: 3,
            pin_system_prompt: true,
            strategy: PruningStrategy::Hybrid,
        }
    }
}

/// Thresholds for the in-loop repetition / error circuit breaker.
/// A scheduler (B) may tune these per unit; they do not belong to B's graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopGuardConfig {
    pub max_history: usize,
    pub repetition_threshold: usize,
    pub hard_repetition_limit: usize,
    pub max_consecutive_errors: usize,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        Self {
            max_history: 10,
            repetition_threshold: 3,
            hard_repetition_limit: 5,
            max_consecutive_errors: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_completion_tokens: Option<usize>,
    /// Maximum loop turns limit (None means unlimited autonomous loop execution)
    pub max_turns: Option<usize>,
    pub max_tokens_budget: Option<usize>,
    pub max_tool_output_bytes: usize,
    pub request_timeout_ms: u64,
    pub thinking_level: Option<String>,
    pub pruning: ContextPruningConfig,
    pub scratchpad: ScratchpadConfig,
    pub loop_guard: LoopGuardConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            system_prompt: Some(DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.to_string()),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            max_turns: None, // Default: no upper bound on loop turns (unlimited)
            max_tokens_budget: None,
            max_tool_output_bytes: 64 * 1024,
            request_timeout_ms: 60_000,
            thinking_level: None,
            pruning: ContextPruningConfig::default(),
            scratchpad: ScratchpadConfig::default(),
            loop_guard: LoopGuardConfig::default(),
        }
    }
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_max_turns(mut self, turns: usize) -> Self {
        self.max_turns = Some(turns);
        self
    }

    pub fn with_unlimited_turns(mut self) -> Self {
        self.max_turns = None;
        self
    }

    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.thinking_level = Some(level.into());
        self
    }

    pub fn with_max_tokens_budget(mut self, budget: usize) -> Self {
        self.max_tokens_budget = Some(budget);
        self
    }

    pub fn with_scratchpad_config(mut self, scratchpad: ScratchpadConfig) -> Self {
        self.scratchpad = scratchpad;
        self
    }

    pub fn with_loop_guard(mut self, loop_guard: LoopGuardConfig) -> Self {
        self.loop_guard = loop_guard;
        self
    }

    /// Isolate this unit's scratch files under `base_dir`.
    /// The actual subdirectory is `base_dir/<agent_id>/` after [`crate::AgentLoop::with_id`].
    pub fn with_scratchpad_dir(mut self, base_dir: impl Into<PathBuf>) -> Self {
        self.scratchpad.base_dir = base_dir.into();
        self
    }
}
