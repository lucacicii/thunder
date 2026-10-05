use crate::cache::warmer::PromptCacheWarmSettings;
use crate::tools::scratchpad::ScratchpadConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Recommended autonomous agent prompt guiding the LLM to call tools when needed and conclude when done.
pub const DEFAULT_AUTONOMOUS_SYSTEM_PROMPT: &str = r#"# Role & Philosophy
You are a rigorous, efficient engineering and task assistant. Your core operating principle is: **"intent first, proceed step by step."**
Before the intent is established, no substantive write or modification may take place.

---

## Phase 1: Intent Gate
Before producing any user-visible content, classify the intent inside an `<intent_analysis>` tag:

1. **Ask (question / consultation)**: the user needs an explanation, a comparison of options, conceptual clarification, or a purely theoretical answer.
2. **Plan (planning / architecture)**: multi-phase goals, a complex refactor or a large feature breakdown, where the top-level design has to be settled first.
3. **Write / Edit (file or content operations)**: creating, modifying or improving a concrete file or code asset.

> **Decision rules**:
> - When the intent is unclear, **always fall back to Ask**.
> - If the user says "change / write / fix" but names no file or gives too little context, classify it as **Write(Ambiguous)**.

---

## Phase 2: Execution Branches

### Branch A: the intent is Ask
1. Answer directly, precisely, and with high information density.
2. **Converge at the end**: close with 2-3 concrete follow-up questions plus preset options (for example, "Would you rather tackle A or B?"), steering the user toward a clear next step.

### Branch B: the intent is Plan
1. Present the goal breakdown, prerequisites, step-by-step approach and risk assessment.
2. Name the key decisions the user has to make, and wait for their confirmation before proceeding.

### Branch C: the intent is Write / Edit
* **Case 1: the goal is clear and the context is complete**
  1. **Read-only probing is allowed**: read-only tools (search, file reads) may be called to gather the necessary context.
  2. **Plan before editing**: state the concrete change plan (affected scope, planned steps).
  3. **Carry out the change**: only once the plan is settled may write/edit tools be called.
* **Case 2: the goal is vague (Write-Ambiguous)**
  1. **Calling write tools is absolutely forbidden.**
  2. Ask 1-2 concrete questions about each missing piece of context (target path, business constraints, compatibility requirements) to get a clear answer.

---

## Runtime Guardrails
1. **Read/write separation**: during the Plan stage only `Read/Search` tools are allowed; `Write/Patch/Delete` tools must never be called before the plan is confirmed.
2. **At most three questions**: follow-ups and clarifications must not exceed 3, must come with concrete options, and vague generic questions are forbidden."#;

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

/// Dynamic toggles for the 4-layer Onion Middleware Pipeline.
/// Allows cleanly disabling transactions, security, or resources in test/benchmark environments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareConfig {
    pub enable_security_guard: bool,
    pub enable_resource_guard: bool,
    pub enable_transaction: bool,
    pub enable_output_post_processor: bool,
}

impl Default for MiddlewareConfig {
    fn default() -> Self {
        let disable_tx = std::env::var("THUNDER_DISABLE_TX")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let disable_sec = std::env::var("THUNDER_DISABLE_SECURITY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Self {
            enable_security_guard: !disable_sec,
            enable_resource_guard: true,
            enable_transaction: !disable_tx,
            enable_output_post_processor: true,
        }
    }
}

/// Tool capability tier for an agent unit.
///
/// Progression: `Read` ⊂ `Write` ⊂ `Bash`.
/// This is enforced by the host (which tools are registered) and by
/// [`crate::tools::middleware::permission_guard::PermissionGuardMiddleware`]
/// (defense in depth). It never leaks into the loop's dialogue logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read-only: `read_file` only.
    Read,
    /// Read + mutate workspace: adds `write_file`.
    Write,
    /// Full: adds `bash`.
    Bash,
}

impl Permission {
    pub fn allows_read(self) -> bool {
        // Every tier may read.
        true
    }

    pub fn allows_write(self) -> bool {
        matches!(self, Self::Write | Self::Bash)
    }

    pub fn allows_exec(self) -> bool {
        matches!(self, Self::Bash)
    }

    /// Whether this tier permits a privileged built-in tool by name.
    ///
    /// Unknown names return `true`: non-builtin tools (plugin / MCP) are gated
    /// by their own layers, not by this ladder.
    pub fn allows_builtin(self, tool_name: &str) -> bool {
        match tool_name {
            "read_file" => self.allows_read(),
            "write_file" => self.allows_write(),
            "bash" => self.allows_exec(),
            _ => true,
        }
    }

    /// Lowercase identifier used in configs, JSONL role files, and IPC payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Bash => "bash",
        }
    }

    /// Human-readable summary injected into prompts / telemetry.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Read => "read-only (fs_write=off, bash=off)",
            Self::Write => "read+write (fs_write=on, bash=off)",
            Self::Bash => "full (fs_write=on, bash=on)",
        }
    }
}

impl Default for Permission {
    /// `Bash` preserves the historical behaviour of every existing caller.
    fn default() -> Self {
        Self::Bash
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
    /// security guard jails paths to the union of primary root + these roots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_workspace_roots: Vec<PathBuf>,
    pub middleware: MiddlewareConfig,
    pub pruning: ContextPruningConfig,
    pub scratchpad: ScratchpadConfig,
    pub loop_guard: LoopGuardConfig,
    /// Tool capability tier for this unit (defaults to `Bash`).
    pub permission: Permission,
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
            system_prompt: Some(DEFAULT_AUTONOMOUS_SYSTEM_PROMPT.to_string()),
            temperature: None,
            top_p: None,
            max_completion_tokens: None,
            max_turns: Some(50), // Default: 50 turns safety ceiling. Use `with_unlimited_turns()` to run indefinitely.
            max_tokens_budget: None,
            max_tool_output_bytes: 64 * 1024,
            request_timeout_ms: 60_000,
            max_stream_retries: 2,
            thinking_level: None,
            workspace_dir: None,
            session_id: None,
            prompt_cache_warm: None,
            extra_workspace_roots: Vec::new(),
            middleware: MiddlewareConfig::default(),
            pruning: ContextPruningConfig::default(),
            scratchpad: ScratchpadConfig::default(),
            loop_guard: LoopGuardConfig::default(),
            permission: Permission::default(),
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
    /// primary workspace (multi-root jail).
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

    /// Disables atomic transaction middleware (e.g. for testing raw tool behavior)
    pub fn without_transactions(mut self) -> Self {
        self.middleware.enable_transaction = false;
        self
    }

    /// Disables all onion middlewares for a zero-overhead raw execution pipeline
    pub fn without_middlewares(mut self) -> Self {
        self.middleware.enable_security_guard = false;
        self.middleware.enable_resource_guard = false;
        self.middleware.enable_transaction = false;
        self.middleware.enable_output_post_processor = false;
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

    /// Set the tool capability tier for this unit.
    pub fn with_permission(mut self, permission: Permission) -> Self {
        self.permission = permission;
        self
    }

    /// Isolate this unit's scratch files under `base_dir`.
    /// The actual subdirectory is `base_dir/<agent_id>/` after [`crate::AgentLoop::with_id`].
    pub fn with_scratchpad_dir(mut self, base_dir: impl Into<PathBuf>) -> Self {
        self.scratchpad.base_dir = base_dir.into();
        self
    }
}
