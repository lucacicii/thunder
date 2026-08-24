use crate::tools::scratchpad::ScratchpadConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub headers: HashMap<String, String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_completion_tokens: Option<usize>,
    /// Maximum loop turns limit (None means unlimited autonomous loop execution)
    pub max_turns: Option<usize>,
    pub max_tokens_budget: Option<usize>,
    pub max_tool_output_bytes: usize,
    pub request_timeout_ms: u64,
    pub pruning: ContextPruningConfig,
    pub scratchpad: ScratchpadConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: None,
            headers: HashMap::new(),
            system_prompt: Some(DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.to_string()),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            max_turns: None, // Default: no upper bound on loop turns (unlimited)
            max_tokens_budget: None,
            max_tool_output_bytes: 64 * 1024,
            request_timeout_ms: 60_000,
            pruning: ContextPruningConfig::default(),
            scratchpad: ScratchpadConfig::default(),
        }
    }
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            ..Default::default()
        }
    }

    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
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

    pub fn with_max_tokens_budget(mut self, budget: usize) -> Self {
        self.max_tokens_budget = Some(budget);
        self
    }

    pub fn with_scratchpad_config(mut self, scratchpad: ScratchpadConfig) -> Self {
        self.scratchpad = scratchpad;
        self
    }
}
