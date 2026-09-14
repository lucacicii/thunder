use crate::config::McpServerConfig;
use crate::error::McpError;
use crate::protocol::{
    CallToolParams, CallToolResult, ImplementationInfo, InitializeParams, InitializeResult,
    JsonRpcNotification, JsonRpcRequest, ListToolsResult, McpTool,
};
use crate::transport::stdio::StdioTransport;
use crate::transport::McpTransport;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

pub struct McpClient {
    server_name: String,
    transport: Arc<dyn McpTransport>,
    server_info: Arc<RwLock<Option<ImplementationInfo>>>,
    request_counter: AtomicU64,
}

impl McpClient {
    /// Connect to an external MCP server process via stdio transport.
    pub async fn connect_stdio(name: impl Into<String>, config: &McpServerConfig) -> Result<Self, McpError> {
        let server_name = name.into();
        let transport = StdioTransport::spawn(&server_name, config).await?;
        let client = Self::from_transport(server_name, Arc::new(transport));

        // Auto initialize
        client.initialize(InitializeParams::default()).await?;

        Ok(client)
    }

    /// Construct an McpClient from any transport implementation.
    pub fn from_transport(server_name: impl Into<String>, transport: Arc<dyn McpTransport>) -> Self {
        Self {
            server_name: server_name.into(),
            transport,
            server_info: Arc::new(RwLock::new(None)),
            request_counter: AtomicU64::new(1),
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    fn next_request_id(&self) -> serde_json::Value {
        serde_json::Value::Number(serde_json::Number::from(
            self.request_counter.fetch_add(1, Ordering::SeqCst),
        ))
    }

    /// Perform protocol initialization handshake.
    pub async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, McpError> {
        info!(server = %self.server_name, "Initializing MCP client handshake");

        let id = self.next_request_id();
        let req = JsonRpcRequest::new(
            id,
            "initialize",
            Some(serde_json::to_value(&params).map_err(|e| McpError::SerializationError(e.to_string()))?),
        );

        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::JsonRpcError {
                code: err.code,
                message: err.message,
                data: err.data,
            });
        }

        let result_val = resp.result.ok_or_else(|| {
            McpError::ProtocolError("Missing result in initialize response".to_string())
        })?;

        let init_result: InitializeResult = serde_json::from_value(result_val)
            .map_err(|e| McpError::ProtocolError(format!("Failed to deserialize initialize result: {e}")))?;

        {
            let mut info_guard = self.server_info.write().await;
            *info_guard = Some(init_result.server_info.clone());
        }

        // Send notifications/initialized notification
        let notify = JsonRpcNotification::new("notifications/initialized", None);
        let _ = self.transport.send_notification(notify).await;

        info!(
            server = %self.server_name,
            protocol_version = %init_result.protocol_version,
            remote_name = %init_result.server_info.name,
            remote_version = %init_result.server_info.version,
            "MCP initialization completed"
        );

        Ok(init_result)
    }

    /// List all tools exposed by the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let id = self.next_request_id();
        let req = JsonRpcRequest::new(id, "tools/list", None);

        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::JsonRpcError {
                code: err.code,
                message: err.message,
                data: err.data,
            });
        }

        let result_val = resp.result.ok_or_else(|| {
            McpError::ProtocolError("Missing result in tools/list response".to_string())
        })?;

        let list_res: ListToolsResult = serde_json::from_value(result_val)
            .map_err(|e| McpError::ProtocolError(format!("Failed to deserialize tools/list result: {e}")))?;

        Ok(list_res.tools)
    }

    /// Invoke a tool on the remote MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let id = self.next_request_id();
        let params = CallToolParams {
            name: tool_name.to_string(),
            arguments,
        };

        let req = JsonRpcRequest::new(
            id,
            "tools/call",
            Some(serde_json::to_value(&params).map_err(|e| McpError::SerializationError(e.to_string()))?),
        );

        let resp = self.transport.send_request(req).await?;

        if let Some(err) = resp.error {
            return Err(McpError::JsonRpcError {
                code: err.code,
                message: err.message,
                data: err.data,
            });
        }

        let result_val = resp.result.ok_or_else(|| {
            McpError::ProtocolError("Missing result in tools/call response".to_string())
        })?;

        let call_res: CallToolResult = serde_json::from_value(result_val)
            .map_err(|e| McpError::ProtocolError(format!("Failed to deserialize tools/call result: {e}")))?;

        Ok(call_res)
    }

    /// Ping the MCP server for health check.
    pub async fn ping(&self) -> Result<(), McpError> {
        let id = self.next_request_id();
        let req = JsonRpcRequest::new(id, "ping", None);
        let resp = self.transport.send_request(req).await?;
        if let Some(err) = resp.error {
            return Err(McpError::JsonRpcError {
                code: err.code,
                message: err.message,
                data: err.data,
            });
        }
        Ok(())
    }

    /// Close connection.
    pub async fn close(&self) -> Result<(), McpError> {
        self.transport.close().await
    }
}
