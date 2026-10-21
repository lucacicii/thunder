use std::fmt;

#[derive(Debug)]
pub enum ProviderError {
    Config(String),
    Auth(String),
    Transport(String),
    NotFound(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "provider config error: {msg}"),
            Self::Auth(msg) => write!(f, "provider auth error: {msg}"),
            Self::Transport(msg) => write!(f, "provider transport error: {msg}"),
            Self::NotFound(msg) => write!(f, "provider not found: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}
