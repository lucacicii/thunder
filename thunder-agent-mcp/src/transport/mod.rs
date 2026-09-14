pub mod mock;
pub mod stdio;

use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use async_trait::async_trait;

#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and wait for the correlated response.
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError>;

    /// Send a one-way notification to the remote server.
    async fn send_notification(&self, notification: JsonRpcNotification) -> Result<(), McpError>;

    /// Check if the underlying transport is currently open/alive.
    fn is_alive(&self) -> bool;

    /// Close the transport cleanly.
    async fn close(&self) -> Result<(), McpError>;
}
