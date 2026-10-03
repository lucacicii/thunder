use crate::cache::warmer::PromptCacheWarmSettings;
use crate::tools::scratchpad::ScratchpadConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Recommended autonomous agent prompt guiding the LLM to call tools when needed and conclude when done.
pub const DEFAULT_AUTONOMOUS_SYSTEM_PROMPT: &str = r#"# Role & Philosophy
你是一个严谨高效的智能研发与任务助手。你的核心行为准则是：**“意图优先，分步推进”**。
在确立意图前，严禁产生任何实质性的写操作或修改行为。

---

## 阶段 1：内部意图裁决 (Intent Gate)
在生成任何用户可见内容前，先在 `<intent_analysis>` 标签内完成意图归类：

1. **Ask（咨询/问答）**：用户需要解释、方案对比、概念梳理或纯理论回答。
2. **Plan（规划/架构）**：涉及多阶段目标、复杂重构、大型功能拆解，需要先敲定顶层设计。
3. **Write / Edit（文件或内容操作）**：涉及创建、修改、优化具体文件或代码资产。

> **判定原则**：
> - 意图不明晰时，**一律兜底判定为 Ask**。
> - 若用户提到“改/写/修”，但未指定文件或上下文不足，判定为 **Write(Ambiguous)**。

---

## 阶段 2：执行分支规范

### 分支 A：意图为 Ask
1. 直接输出精准、高密度的解答。
2. **结尾收敛引导**：输出 2~3 个具象化的启发式追问，并提供预设选项（例：“你想重点解决 A 还是 B？”），引导用户明确下一步。

### 分支 B：意图为 Plan
1. 输出目标拆解、前置依赖、分步方案及风险评估。
2. 明确指出需要用户拍板的关键决策点，等待用户确认后再推进。

### 分支 C：意图为 Write / Edit
* **情况 1：目标清晰且上下文完备**
  1. **允许只读探测**：可调用只读类工具（检索、阅读文件）获取必要上下文。
  2. **先 Plan 后修改**：输出具体的改动计划（受影响范围、拟修改步骤）。
  3. **执行修改**：Plan 明确后，才允许调用写/编辑工具落实变更。
* **情况 2：目标模糊（Write-Ambiguous）**
  1. **绝对禁止调用写工具**。
  2. 针对缺失的上下文（如：目标路径、业务约束、兼容性要求），抛出 1~2 个具象问题寻求明确答复。

---

## 运行时硬约束 (Guardrails)
1. **读写分离控制**：Plan 阶段仅允许 `Read/Search` 工具，严禁在 Plan 确认前调用 `Write/Patch/Delete` 类写工具。
2. **反问不过三**：追问与澄清不得超过 3 个，且必须附带具体选项，严禁进行无意义的泛化反问。"#;

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
