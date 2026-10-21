use crate::cache::warmer::PromptCacheWarmSettings;
use crate::tools::scratchpad::ScratchpadConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// In-memory context compaction policy.
///
/// The loop ships no default system prompt: what the agent should be told is
/// the host's business, and a capability pack may supply one (see
/// `thunder-agent-pack-code`'s engineering prompt). `AgentConfig::system_prompt`
/// of `None` means the host passes the system message itself, or the model runs
/// without one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPruningConfig {
    pub max_context_tokens: usize,
    /// Tokens reserved for the model's response. Compaction triggers when
    /// `estimated > max_context_tokens - reserve_tokens`.
    #[serde(default = "default_reserve_tokens")]
    pub reserve_tokens: usize,
    /// Newest tokens kept verbatim (never summarized).
    #[serde(default = "default_keep_recent_tokens")]
    pub keep_recent_tokens: usize,
    /// Optional smaller/faster model dedicated to checkpoint summarization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarizer_model: Option<String>,
    /// Output cap for the one-off summarization call.
    #[serde(default = "default_summarizer_max_tokens")]
    pub summarizer_max_tokens: usize,
    /// Keep the leading system prompt pinned during compaction.
    pub pin_system_prompt: bool,
}

fn default_reserve_tokens() -> usize {
    16_384
}

fn default_keep_recent_tokens() -> usize {
    20_000
}

fn default_summarizer_max_tokens() -> usize {
    4096
}

impl Default for ContextPruningConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            reserve_tokens: default_reserve_tokens(),
            keep_recent_tokens: default_keep_recent_tokens(),
            summarizer_model: None,
            summarizer_max_tokens: default_summarizer_max_tokens(),
            pin_system_prompt: true,
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
    /// Ceiling on bytes queued in this run's event stream.
    ///
    /// The channel is bounded by frame count; this bounds the memory those
    /// frames can pin when one of them carries a large tool result. A producer
    /// that cannot get space in five seconds drops the event and counts it.
    pub max_event_bytes: usize,
    pub request_timeout_ms: u64,
    pub max_stream_retries: usize,
    pub thinking_level: Option<String>,
    pub workspace_dir: Option<PathBuf>,
    /// Stable per-conversation id forwarded to the transport as pi-ai's
    /// `options.sessionId`: it becomes OpenAI's `prompt_cache_key`, Mistral's
    /// `promptCacheKey`, and Anthropic-compatible session-affinity headers,
    /// routing every request of one conversation onto the same prompt-cache
    /// shard. `None` disables affinity (one-off calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Prompt-cache warming policy for this run. `None` (default) disables
    /// warming. Hosts derive it from the resolved `ModelSpec`
    /// (`promptCache` lifetime + `cost` pricing must both be declared).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_warm: Option<PromptCacheWarmSettings>,
    /// Additional workspace roots (e.g. repositories referenced by the task)
    /// granted the same read/write standing as the primary workspace. The
    /// the host's scope treats them as inside the working area.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_workspace_roots: Vec<PathBuf>,
    pub pruning: ContextPruningConfig,
    pub scratchpad: ScratchpadConfig,
    pub loop_guard: LoopGuardConfig,
    /// Identifies this run for services shared across runs (see
    /// [`crate::types::tool::ToolExecutionContext::route`]).
    ///
    /// Defaults to `None`, which means "no route": shared services then fall back
    /// to their most restrictive behaviour rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            max_turns: Some(50), // Default: 50 turns safety ceiling. Use `with_unlimited_turns()` to run indefinitely.
            max_tokens_budget: None,
            max_tool_output_bytes: 64 * 1024,
            max_event_bytes: crate::loop_engine::event_stream::DEFAULT_MAX_EVENT_BYTES,
            request_timeout_ms: 60_000,
            max_stream_retries: 2,
            thinking_level: None,
            workspace_dir: None,
            session_id: None,
            prompt_cache_warm: None,
            extra_workspace_roots: Vec::new(),
            pruning: ContextPruningConfig::default(),
            scratchpad: ScratchpadConfig::default(),
            loop_guard: LoopGuardConfig::default(),
            route: None,
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

    pub fn with_workspace_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_dir = Some(path.into());
        self
    }

    /// Grant additional workspace roots the same read/write standing as the
    /// primary workspace.
    pub fn with_extra_workspace_roots(
        mut self,
        roots: impl IntoIterator<Item = impl Into<PathBuf>>,
    ) -> Self {
        self.extra_workspace_roots
            .extend(roots.into_iter().map(Into::into));
        self
    }

    pub fn with_max_stream_retries(mut self, retries: usize) -> Self {
        self.max_stream_retries = retries;
        self
    }

    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.thinking_level = Some(level.into());
        self
    }

    /// Bind all requests of this run to one prompt-cache routing key.
    /// Use the conversation id so resumed conversations keep their affinity.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
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
