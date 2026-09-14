use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::transport::McpTransport;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

type HandlerFn = Box<dyn Fn(JsonRpcRequest) -> Result<JsonRpcResponse, McpError> + Send + Sync>;

pub struct MockTransport {
    handler: Arc<Mutex<Option<HandlerFn>>>,
    is_alive: Arc<AtomicBool>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            handler: Arc::new(Mutex::new(None)),
            is_alive: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn with_handler<F>(handler: F) -> Self
    where
        F: Fn(JsonRpcRequest) -> Result<JsonRpcResponse, McpError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(Mutex::new(Some(Box::new(handler)))),
            is_alive: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpTransport for MockTransport {
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        if !self.is_alive() {
            return Err(McpError::NotConnected("Mock transport closed".to_string()));
        }

        let guard = self.handler.lock().await;
        if let Some(h) = guard.as_ref() {
            h(request)
        } else {
            // Default mock responses for standard MCP methods
            let resp = match request.method.as_str() {
                "initialize" => JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-mcp-server", "version": "1.0.0" }
                    })),
                    error: None,
                },
                "tools/list" => JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({
                        "tools": [
                            {
                                "name": "mock_fetch_data",
                                "description": "Fetches mock data from remote server",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "query": { "type": "string" }
                                    },
                                    "required": ["query"]
                                }
                            }
                        ]
                    })),
                    error: None,
                },
                "tools/call" => {
                    let tool_name = request.params.as_ref()
                        .and_then(|p| p.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");

                    JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: Some(serde_json::json!({
                            "content": [
                                { "type": "text", "text": format!("Mock response from {tool_name}") }
                            ],
                            "isError": false
                        })),
                        error: None,
                    }
                }
                "ping" => JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::json!({})),
                    error: None,
                },
                other => JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: None,
                    error: Some(crate::protocol::JsonRpcErrorDetail {
                        code: -32601,
                        message: format!("Method not found: {other}"),
                        data: None,
                    }),
                },
            };
            Ok(resp)
        }
    }

    async fn send_notification(&self, _notification: JsonRpcNotification) -> Result<(), McpError> {
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::SeqCst)
    }

    async fn close(&self) -> Result<(), McpError> {
        self.is_alive.store(false, Ordering::SeqCst);
        Ok(())
    }
}
