use std::fmt;

#[derive(Debug, Clone)]
pub enum PluginError {
    InitFailed(String),
    ExecutionFailed(String),
    NotFound(String),
    RegistrationConflict(String),
    Serialization(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitFailed(msg) => write!(f, "Plugin initialization failed: {msg}"),
            Self::ExecutionFailed(msg) => write!(f, "Plugin execution failed: {msg}"),
            Self::NotFound(id) => write!(f, "Plugin not found: {id}"),
            Self::RegistrationConflict(id) => write!(f, "Plugin registration conflict for id: {id}"),
            Self::Serialization(msg) => write!(f, "Plugin serialization error: {msg}"),
        }
    }
}

impl std::error::Error for PluginError {}
