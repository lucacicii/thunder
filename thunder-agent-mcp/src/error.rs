#[derive(Debug)]
pub enum McpError {
    IoError(String),
    ProtocolError(String),
    JsonRpcError {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
    TransportError(String),
    ProcessFailed(String),
    ToolNotFound(String),
    ServerNotFound(String),
    SerializationError(String),
    Timeout(String),
    NotConnected(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(msg) => write!(f, "MCP IO error: {msg}"),
            Self::ProtocolError(msg) => write!(f, "MCP protocol error: {msg}"),
            Self::JsonRpcError { code, message, .. } => {
                write!(f, "MCP JSON-RPC error ({code}): {message}")
            }
            Self::TransportError(msg) => write!(f, "MCP transport error: {msg}"),
            Self::ProcessFailed(msg) => write!(f, "MCP process failed: {msg}"),
            Self::ToolNotFound(msg) => write!(f, "MCP tool not found: {msg}"),
            Self::ServerNotFound(msg) => write!(f, "MCP server not found: {msg}"),
            Self::SerializationError(msg) => write!(f, "MCP serialization error: {msg}"),
            Self::Timeout(msg) => write!(f, "MCP request timeout: {msg}"),
            Self::NotConnected(msg) => write!(f, "MCP client not connected: {msg}"),
        }
    }
}

impl std::error::Error for McpError {}

impl From<std::io::Error> for McpError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err.to_string())
    }
}

impl From<serde_json::Error> for McpError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}
