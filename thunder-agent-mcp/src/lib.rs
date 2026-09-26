//! # Thunder Agent MCP
//!
//! Model Context Protocol (MCP) client, configuration parser, and dynamic tool bridge for Thunder Agent.
//! Supports standard JSON-RPC 2.0 stdio transports, config parsing, tool discovery, and runtime
//! `AgentTool` integration into `thunder-agent-loop`.

pub mod client;
pub mod config;
pub mod error;
pub mod manager;
pub mod protocol;
pub mod tool_bridge;
pub mod transport;

pub mod prelude {
    pub use crate::client::McpClient;
    pub use crate::config::{McpConfig, McpServerConfig};
    pub use crate::error::McpError;
    pub use crate::manager::McpManager;
    pub use crate::protocol::{
        CallToolResult, ImplementationInfo, InitializeParams, InitializeResult, McpTool,
        ToolContent,
    };
    pub use crate::tool_bridge::McpToolBridge;
    pub use crate::transport::mock::MockTransport;
    pub use crate::transport::stdio::StdioTransport;
    pub use crate::transport::McpTransport;
}

pub use prelude::*;
