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
            max_stream_retries: 2,
            thinking_level: None,
            workspace_dir: None,
            extra_workspace_roots: Vec::new(),
            middleware: MiddlewareConfig::default(),
            pruning: ContextPruningConfig::default(),
            scratchpad: ScratchpadConfig::default(),
            loop_guard: LoopGuardConfig::default(),
            permission: Permission::default(),
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
        self.extra_workspace_roots.extend(roots.into_iter().map(Into::into));
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
