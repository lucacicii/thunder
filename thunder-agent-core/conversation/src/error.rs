use std::fmt;

#[derive(Debug)]
pub enum ConversationError {
    NotFound(String),
    AlreadyExists(String),
    StoreError(String),
    Io(std::io::Error),
    Serialization(serde_json::Error),
    InvalidOperation(String),
}

impl fmt::Display for ConversationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "Conversation `{id}` not found"),
            Self::AlreadyExists(id) => write!(f, "Conversation `{id}` already exists"),
            Self::StoreError(msg) => write!(f, "Store error: {msg}"),
            Self::Io(err) => write!(f, "IO error: {err}"),
            Self::Serialization(err) => write!(f, "Serialization error: {err}"),
            Self::InvalidOperation(msg) => write!(f, "Invalid operation: {msg}"),
        }
    }
}

impl std::error::Error for ConversationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serialization(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ConversationError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for ConversationError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err)
    }
}
