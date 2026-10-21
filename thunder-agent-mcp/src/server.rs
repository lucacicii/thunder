use crate::protocol::{
    CallToolParams, CallToolResult as McpCallToolResult, ImplementationInfo, InitializeResult,
    JsonRpcErrorDetail, JsonRpcRequest, JsonRpcResponse, McpTool, ServerCapabilities,
    ToolContent as McpToolContent, LATEST_PROTOCOL_VERSION,
};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;
use thunder_agent_policy::plugin::{ToolContent, ToolPlugin};
use thunder_agent_types::tool::ProgressReporter;
use thunder_agent_policy::ToolExecutionContext;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Progress reporter that sends MCP `notifications/progress` notifications.
#[derive(Clone, Debug)]
pub struct McpStreamProgressReporter {
    token: serde_json::Value,
    sender: tokio::sync::mpsc::UnboundedSender<String>,
}

#[async_trait]
impl ProgressReporter for McpStreamProgressReporter {
    async fn report_progress(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": self.token.clone(),
                "progress": progress,
                "total": total,
                "message": message,
            }
        });
        let _ = self.sender.send(notif.to_string());
    }
}

/// An MCP Server that exposes local `ToolPlugin` implementations to any MCP-compliant client.
///
/// Can be served over stdio (for IDEs / Cursor / Claude Desktop) or invoked directly via JSON-RPC.
#[derive(Clone)]
pub struct McpServer {
    info: ImplementationInfo,
    tools: BTreeMap<String, Arc<dyn ToolPlugin>>,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new("thunder-mcp-server", "0.1.0")
    }
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            info: ImplementationInfo {
                name: name.into(),
                version: version.into(),
            },
            tools: BTreeMap::new(),
        }
    }

    /// Register a `ToolPlugin` into this MCP server.
    pub fn register_tool(&mut self, tool: Arc<dyn ToolPlugin>) {
        let name = tool.definition().function.name;
        self.tools.insert(name, tool);
    }

    /// Builder method to register a tool.
    pub fn with_tool(mut self, tool: Arc<dyn ToolPlugin>) -> Self {
        self.register_tool(tool);
        self
    }

    /// Bulk register multiple tools.
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = Arc<dyn ToolPlugin>>) -> Self {
        for tool in tools {
            self.register_tool(tool);
        }
        self
    }

    /// List all registered tools in MCP format.
    pub fn list_tools(&self) -> Vec<McpTool> {
        self.tools
            .values()
            .map(|t| {
                let def = t.definition();
                McpTool {
                    name: def.function.name,
                    description: Some(def.function.description),
                    input_schema: def.function.parameters,
                }
            })
            .collect()
    }

    /// Handle a single parsed JSON-RPC request and return a response (if not a notification).
    pub async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        self.handle_request_with_progress(req, None).await
    }

    /// Handle a request with an optional channel for streaming `notifications/progress` notifications.
    pub async fn handle_request_with_progress(
        &self,
        req: JsonRpcRequest,
        progress_sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Option<JsonRpcResponse> {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => {
                let result = InitializeResult {
                    protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
                    capabilities: ServerCapabilities {
                        tools: Some(serde_json::json!({})),
                        resources: None,
                        prompts: None,
                        logging: None,
                    },
                    server_info: self.info.clone(),
                };
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(serde_json::to_value(result).unwrap_or_default()),
                    error: None,
                })
            }
            "notifications/initialized" | "initialized" => {
                // Client acknowledgment notification: no response
                None
            }
            "ping" => Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({})),
                error: None,
            }),
            "tools/list" => {
                let tools = self.list_tools();
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(serde_json::json!({ "tools": tools })),
                    error: None,
                })
            }
            "tools/call" => {
                let params: Result<CallToolParams, _> = match req.params {
                    Some(p) => serde_json::from_value(p),
                    None => Err(serde_json::Error::io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Missing params",
                    ))),
                };

                let params = match params {
                    Ok(p) => p,
                    Err(e) => {
                        return Some(JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: None,
                            error: Some(JsonRpcErrorDetail {
                                code: -32602,
                                message: format!("Invalid params: {e}"),
                                data: None,
                            }),
                        });
                    }
                };

                let tool = match self.tools.get(&params.name) {
                    Some(t) => t.clone(),
                    None => {
                        return Some(JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: None,
                            error: Some(JsonRpcErrorDetail {
                                code: -32601,
                                message: format!("Tool not found: {}", params.name),
                                data: None,
                            }),
                        });
                    }
                };

                let args = params.arguments.clone().unwrap_or(serde_json::json!({}));
                let mut ctx = ToolExecutionContext::default();

                if let (Some(token), Some(tx)) = (params.progress_token(), progress_sender) {
                    ctx = ctx.with_progress_reporter(Arc::new(McpStreamProgressReporter {
                        token,
                        sender: tx,
                    }));
                }

                match tool.execute(args, &ctx).await {
                    Ok(res) => {
                        let mcp_res = McpCallToolResult {
                            content: res
                                .content
                                .into_iter()
                                .map(|c| match c {
                                    ToolContent::Text { text } => McpToolContent::Text { text },
                                    ToolContent::Image { data, mime_type } => {
                                        McpToolContent::Image { data, mime_type }
                                    }
                                    ToolContent::Resource { uri, text } => {
                                        McpToolContent::Resource { uri, text }
                                    }
                                })
                                .collect(),
                            is_error: res.is_error,
                        };
                        Some(JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(serde_json::to_value(mcp_res).unwrap_or_default()),
                            error: None,
                        })
                    }
                    Err(err) => Some(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(serde_json::json!({
                            "content": [{ "type": "text", "text": err }],
                            "isError": true
                        })),
                        error: None,
                    }),
                }
            }
            unknown => Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcErrorDetail {
                    code: -32601,
                    message: format!("Method not found: {unknown}"),
                    data: None,
                }),
            }),
        }
    }

    /// Handle raw JSON-RPC string message (e.g. from line-based stdio transport).
    pub async fn handle_raw_line(&self, line: &str) -> Option<String> {
        self.handle_raw_line_with_progress(line, None).await
    }

    /// Handle raw JSON-RPC string message with optional progress notification stream.
    pub async fn handle_raw_line_with_progress(
        &self,
        line: &str,
        progress_sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: serde_json::Value::Null,
                    result: None,
                    error: Some(JsonRpcErrorDetail {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                        data: None,
                    }),
                };
                return serde_json::to_string(&err_resp).ok();
            }
        };

        let resp = self
            .handle_request_with_progress(req, progress_sender)
            .await?;
        serde_json::to_string(&resp).ok()
    }

    /// Run the MCP server over standard input and output streams with full progress streaming.
    pub async fn serve<R, W>(&self, reader: R, mut writer: W) -> Result<(), std::io::Error>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let writer_handle = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                writer.write_all(msg.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
            Ok::<(), std::io::Error>(())
        });

        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            let server = self.clone();
            let out = out_tx.clone();
            tokio::spawn(async move {
                if let Some(resp) = server
                    .handle_raw_line_with_progress(&line, Some(out.clone()))
                    .await
                {
                    let _ = out.send(resp);
                }
            });
        }

        drop(out_tx);
        let _ = writer_handle.await;
        Ok(())
    }
}
