use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// Unified internal tool namespace URI representation (§4.2).
///
/// Priority order for shadowing:
/// `Local Script (3) > External MCP (2) > Core (1)`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolNamespace {
    /// `core://<name>` -> exposed as `core__<name>`
    Core { name: String },
    /// `mcp://<server>/<name>` -> exposed as `mcp__<server>__<name>`
    Mcp { server: String, name: String },
    /// `script://<name>` -> exposed as `script__<name>`
    Script { name: String },
}

impl ToolNamespace {
    /// Parse an internal URI such as `core://read_file`, `mcp://git/commit`, or `script://squash`.
    pub fn parse_uri(uri: &str) -> Option<Self> {
        if let Some(rest) = uri.strip_prefix("core://") {
            let name = sanitize_identifier(rest);
            if !name.is_empty() {
                return Some(Self::Core { name });
            }
        } else if let Some(rest) = uri.strip_prefix("mcp://") {
            let (server, name) = rest.split_once('/')?;
            let server = sanitize_identifier(server);
            let name = sanitize_identifier(name);
            if !server.is_empty() && !name.is_empty() {
                return Some(Self::Mcp { server, name });
            }
        } else if let Some(rest) = uri.strip_prefix("script://") {
            let name = sanitize_identifier(rest);
            if !name.is_empty() {
                return Some(Self::Script { name });
            }
        }
        None
    }

    /// Format into the canonical internal URI.
    pub fn to_uri(&self) -> String {
        match self {
            Self::Core { name } => format!("core://{name}"),
            Self::Mcp { server, name } => format!("mcp://{server}/{name}"),
            Self::Script { name } => format!("script://{name}"),
        }
    }

    /// Flatten into an LLM-safe tool name (`[a-zA-Z0-9_-]`).
    pub fn exposed_name(&self) -> String {
        match self {
            Self::Core { name } => format!("core__{}", sanitize_identifier(name)),
            Self::Mcp { server, name } => format!(
                "mcp__{}__{}",
                sanitize_identifier(server),
                sanitize_identifier(name)
            ),
            Self::Script { name } => format!("script__{}", sanitize_identifier(name)),
        }
    }

    /// The logical leaf tool name (used for shadowing comparisons across namespaces).
    pub fn logical_name(&self) -> &str {
        match self {
            Self::Core { name } | Self::Mcp { name, .. } | Self::Script { name } => name.as_str(),
        }
    }

    /// Shadowing priority (`Local Script > External MCP > Core`).
    pub fn priority(&self) -> u8 {
        match self {
            Self::Core { .. } => 1,
            Self::Mcp { .. } => 2,
            Self::Script { .. } => 3,
        }
    }
}

/// Sanitize a component into a valid LLM tool identifier segment.
pub fn sanitize_identifier(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// What a tool does to the workspace, declared by the plugin and consumed by the
/// agent loop's stall detector.
///
/// The kernel never interprets tool arguments or output; it only needs one bit of
/// knowledge that it cannot derive itself: whether calling this tool can change
/// state. Everything else (windows, thresholds, exit actions) stays in the core.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    /// Contract: identical arguments imply an identical world state (idempotent).
    /// The agent loop treats `hash(args)` as a snapshot of the state it wrote, so a
    /// repeated call with unchanged content counts as no progress.
    Mutating,
    /// Read-only. An unchanged argument list plus an unchanged output means no new
    /// information was gained.
    Observing,
    /// The side effect cannot be described by the arguments, or the source is not
    /// trusted (shell execution, third-party MCP servers). Only output digests are
    /// used as a fallback signal.
    ///
    /// `bash` **must** stay `Neutral`: it breaks the `Mutating` contract because
    /// `echo x > f` leaves no trace of the write in its arguments.
    #[default]
    Neutral,
}

impl ProgressKind {
    /// Returns true for [`ProgressKind::Neutral`], letting wire structs omit the field.
    pub fn is_default(&self) -> bool {
        *self == ProgressKind::Neutral
    }
}

/// Unified tool schema descriptor (§6.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDescriptor {
    /// Flattened name exposed to the LLM (e.g. `core__read_file`, `mcp__git__commit`, `script__git_squash_feature`).
    pub name: String,
    /// Human- and LLM-readable description of the tool.
    pub description: String,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: Value,
    /// Canonical internal URI (`core://...`, `mcp://.../...`, `script://...`).
    pub uri: String,
    /// Declared by the plugin; drives the core's progress/stall detection.
    #[serde(default)]
    pub progress: ProgressKind,
}

impl ToolDescriptor {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        uri: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            uri: uri.into(),
            progress: ProgressKind::default(),
        }
    }

    pub fn with_progress(mut self, progress: ProgressKind) -> Self {
        self.progress = progress;
        self
    }

    pub fn from_namespace(
        ns: &ToolNamespace,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: ns.exposed_name(),
            description: description.into(),
            input_schema,
            uri: ns.to_uri(),
            progress: ProgressKind::default(),
        }
    }

    pub fn new_core(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        let ns = ToolNamespace::Core {
            name: sanitize_identifier(&name.into()),
        };
        Self::from_namespace(&ns, description, input_schema)
    }

    pub fn new_mcp(
        server: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        let ns = ToolNamespace::Mcp {
            server: sanitize_identifier(&server.into()),
            name: sanitize_identifier(&name.into()),
        };
        Self::from_namespace(&ns, description, input_schema)
    }

    pub fn new_script(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        let ns = ToolNamespace::Script {
            name: sanitize_identifier(&name.into()),
        };
        Self::from_namespace(&ns, description, input_schema)
    }

    pub fn namespace(&self) -> Option<ToolNamespace> {
        ToolNamespace::parse_uri(&self.uri)
    }

    pub fn priority(&self) -> u8 {
        self.namespace().map(|ns| ns.priority()).unwrap_or(0)
    }

    pub fn logical_name(&self) -> String {
        self.namespace()
            .map(|ns| ns.logical_name().to_string())
            .unwrap_or_else(|| self.name.clone())
    }
}

/// Per-call execution context passed down to providers and sandboxes (§6.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionContext {
    pub session_id: String,
    pub workspace_cwd: PathBuf,
    pub timeout: Duration,
}

impl ExecutionContext {
    pub fn new(
        session_id: impl Into<String>,
        workspace_cwd: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            workspace_cwd: workspace_cwd.into(),
            timeout,
        }
    }
}

/// Low-level provider/executor error before fault normalization (§6.1).
#[derive(Debug, Error)]
pub enum ToolExecutionError {
    #[error("Tool execution timed out")]
    Timeout,

    #[error("Tool exited with status {code}: {stderr}")]
    NonZeroExit { code: i32, stderr: String },

    #[error("I/O error: {0}")]
    IOError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),
}

/// Normalized execution outcome for the Agent Loop state machine (§5.3).
///
/// - `Success`: normal tool output fed back to the LLM.
/// - `ToolError`: tool-level error (bad args, file not found, non-zero script exit) fed back to the LLM for self-correction.
/// - `SystemFault`: infrastructure fault (broken pipe, missing binary, timeout kill) handled by the kernel / supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionResult {
    Success {
        output: String,
    },
    ToolError {
        code: i32,
        stderr: String,
        is_retryable: bool,
    },
    SystemFault {
        reason: String,
    },
}

impl ToolExecutionResult {
    /// Normalize a low-level `Result<String, ToolExecutionError>` into a `ToolExecutionResult` (§5.3).
    pub fn normalize(result: Result<String, ToolExecutionError>) -> Self {
        match result {
            Ok(output) => Self::Success { output },
            Err(ToolExecutionError::NonZeroExit { code, stderr }) => Self::ToolError {
                code,
                stderr,
                is_retryable: true,
            },
            Err(ToolExecutionError::SerializationError(msg)) => Self::ToolError {
                code: -32602,
                stderr: format!("Invalid tool arguments or serialization error: {msg}"),
                is_retryable: true,
            },
            Err(ToolExecutionError::ToolNotFound(name)) => Self::ToolError {
                code: -32601,
                stderr: format!("Tool '{name}' is not registered"),
                is_retryable: false,
            },
            Err(ToolExecutionError::Timeout) => Self::SystemFault {
                reason: "Execution timed out; process group terminated with SIGKILL".to_string(),
            },
            Err(ToolExecutionError::IOError(err)) => Self::SystemFault {
                reason: format!("Process/Pipe I/O fault ({:?}): {}", err.kind(), err),
            },
            Err(ToolExecutionError::ProtocolError(reason)) => Self::SystemFault {
                reason: format!("RPC/Wire protocol fault: {reason}"),
            },
        }
    }

    /// Format the normalized result into a string suitable for an LLM tool result message.
    pub fn to_llm_message(&self) -> String {
        match self {
            Self::Success { output } => output.clone(),
            Self::ToolError {
                code,
                stderr,
                is_retryable,
            } => format!("[ToolExecutionError code={code} retryable={is_retryable}]\n{stderr}"),
            Self::SystemFault { reason } => {
                format!("[SystemFault]\n{reason}")
            }
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

/// Abstract tool provider contract implemented by MCP clients and TSP script runners (§6.1).
#[async_trait]
pub trait ToolProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError>;
}
