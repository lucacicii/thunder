use std::fmt;

/// Recoverable unit-level failure. Terminal loop outcomes such as
/// cancellation, budget, or max-turns stay on [`crate::FinishReason`]
/// inside [`crate::AgentRunResult`] — they are not errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// A second `start`/`run` was issued while this unit is already executing.
    AlreadyRunning { agent_id: String },
    /// The spawned loop ended without producing a result (panic or dropped).
    Terminated { agent_id: String },
    /// Host-supplied configuration cannot be used.
    Config(String),
    /// Catch-all for unexpected internal failures.
    Other(String),
}

impl AgentError {
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::AlreadyRunning { agent_id } | Self::Terminated { agent_id } => Some(agent_id),
            Self::Config(_) | Self::Other(_) => None,
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { agent_id } => {
                write!(f, "agent `{agent_id}` is already running a task")
            }
            Self::Terminated { agent_id } => {
                write!(f, "agent `{agent_id}` terminated without a result")
            }
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<String> for AgentError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

impl From<&str> for AgentError {
    fn from(value: &str) -> Self {
        Self::Other(value.to_string())
    }
}
