use crate::tools::{dispatch_tool_call, std_tool_definitions};
use serde_json::{json, Value};
use thunder_protocol::{JsonRpcRequest, JsonRpcResponse, McpListToolsResult, McpToolCallParams};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Handle a single JSON-RPC 2.0 request frame and return `Some(JsonRpcResponse)` (or `None` for notifications).
pub async fn handle_rpc_frame(line: &str) -> Option<JsonRpcResponse> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let raw_val: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                Value::Null,
                -32700,
                format!("Parse error: {e}"),
            ));
        }
    };

    // Notifications do not carry an `id` field.
    raw_val.get("id")?;

    let req: JsonRpcRequest = match serde_json::from_value(raw_val) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                Value::Null,
                -32600,
                format!("Invalid Request: {e}"),
            ));
        }
    };

    match req.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            req.id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "thunder-std",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "ping" => Some(JsonRpcResponse::success(req.id, json!({}))),
        "tools/list" => {
            let result = McpListToolsResult {
                tools: std_tool_definitions(),
            };
            Some(JsonRpcResponse::success(
                req.id,
                serde_json::to_value(result).unwrap_or_else(|_| json!({"tools": []})),
            ))
        }
        "tools/call" => {
            let Some(params_val) = req.params else {
                return Some(JsonRpcResponse::error(
                    req.id,
                    -32602,
                    "Missing params for tools/call",
                ));
            };
            let call_params: McpToolCallParams = match serde_json::from_value(params_val) {
                Ok(p) => p,
                Err(e) => {
                    return Some(JsonRpcResponse::error(
                        req.id,
                        -32602,
                        format!("Invalid tools/call params: {e}"),
                    ));
                }
            };
            let call_res = dispatch_tool_call(call_params).await;
            Some(JsonRpcResponse::success(
                req.id,
                serde_json::to_value(call_res).unwrap_or(Value::Null),
            ))
        }
        other => Some(JsonRpcResponse::error(
            req.id,
            -32601,
            format!("Method not found: {other}"),
        )),
    }
}

/// Run the `thunder-std` MCP Server loop over any async reader/writer pair (Stdio in production).
pub async fn run_stdio_server<R, W>(input: R, mut output: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(input).lines();
    while let Some(line) = reader.next_line().await? {
        if let Some(resp) = handle_rpc_frame(&line).await {
            let serialized = serde_json::to_string(&resp).map_err(std::io::Error::other)?;
            output.write_all(serialized.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }
    Ok(())
}
