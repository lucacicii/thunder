use crate::db::Database;
use crate::search::MemorySearchEngine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpListToolsResult {
    pub tools: Vec<McpToolInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpCallToolResult {
    pub content: Vec<McpContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

impl McpCallToolResult {
    pub fn success_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContent::Text {
                text: text.into(),
            }],
            is_error: false,
        }
    }

    pub fn error_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContent::Text {
                text: text.into(),
            }],
            is_error: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

pub struct MemoryMcpServer {
    db: Database,
}

impl MemoryMcpServer {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub fn tool_definitions() -> Vec<McpToolInfo> {
        vec![
            McpToolInfo {
                name: "memory_query".into(),
                description: Some("Search long-term memories and desktop activity history using cognitive relevance, importance, and recency scoring.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Keywords or question about past activity, code, errors, or documentation."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of memory items to return (default: 5)."
                        }
                    },
                    "required": ["query"]
                }),
            },
            McpToolInfo {
                name: "memory_recent".into(),
                description: Some("List recent long-term memories, developer tasks, and consolidated desktop insights.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of recent memories to return (default: 5)."
                        }
                    }
                }),
            },
            McpToolInfo {
                name: "memory_current_screen".into(),
                description: Some("Retrieve the latest recorded screen perception excerpt (active application, window title, OCR text) to know what the user is currently looking at.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpToolInfo {
                name: "memory_stats".into(),
                description: Some("Get statistics of recorded screen captures, consolidated long-term memories, and training samples.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        ]
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        match name {
            "memory_query" => {
                let query = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing 'query' parameter".to_string())?;
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5) as usize;

                let searcher = MemorySearchEngine::new(self.db.clone());
                let scored = searcher
                    .query(query, limit)
                    .map_err(|e| e.to_string())?;

                if scored.is_empty() {
                    return Ok(format!("No long-term memories found matching '{}'.", query));
                }

                let mut out = format!("### Long-Term Memory Search Results for `{}`:\n\n", query);
                for (i, item) in scored.iter().enumerate() {
                    let m = &item.memory;
                    out.push_str(&format!(
                        "**{}. [{}] {}** (Relevance: {:.2}, Importance: {:.1}/10)\n",
                        i + 1,
                        m.category,
                        m.title,
                        item.relevance_score,
                        m.importance_score
                    ));
                    out.push_str(&format!("- **Summary**: {}\n", m.summary));
                    if !m.key_entities.is_empty() {
                        out.push_str(&format!("- **Entities**: {}\n", m.key_entities.join(", ")));
                    }
                    out.push('\n');
                }
                Ok(out)
            }

            "memory_recent" => {
                let limit = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5) as usize;

                let memories = self
                    .db
                    .list_recent_memories(limit)
                    .map_err(|e| e.to_string())?;

                if memories.is_empty() {
                    return Ok("No long-term memories recorded yet.".into());
                }

                let mut out = String::from("### Recent Long-Term Memories:\n\n");
                for (i, m) in memories.iter().enumerate() {
                    out.push_str(&format!(
                        "**{}. [{}] {}** (Score: {:.1}/10)\n",
                        i + 1,
                        m.category,
                        m.title,
                        m.importance_score
                    ));
                    out.push_str(&format!("- {}\n", m.summary));
                    if !m.key_entities.is_empty() {
                        out.push_str(&format!("- **Entities**: {}\n", m.key_entities.join(", ")));
                    }
                    out.push('\n');
                }
                Ok(out)
            }

            "memory_current_screen" => {
                let captures = self
                    .db
                    .get_unprocessed_captures(1)
                    .map_err(|e| e.to_string())?;

                if let Some(c) = captures.first() {
                    let mut out = format!("### Current Desktop Perception Context:\n");
                    out.push_str(&format!("- **Active App**: `{}`\n", c.app_name));
                    out.push_str(&format!("- **Window Title**: `{}`\n", c.window_title));
                    let excerpt = if c.ocr_text.len() > 1000 {
                        format!("{}...", &c.ocr_text[..1000])
                    } else {
                        c.ocr_text.clone()
                    };
                    out.push_str(&format!("- **Screen Text Excerpt**:\n```\n{}\n```\n", excerpt));
                    Ok(out)
                } else {
                    let stats = self.db.get_stats().map_err(|e| e.to_string())?;
                    Ok(format!(
                        "No new unhandled captures. Total recorded screen frames: {}.",
                        stats.total_captures
                    ))
                }
            }

            "memory_stats" => {
                let stats = self.db.get_stats().map_err(|e| e.to_string())?;
                let out = format!(
                    "### Thunder Memory Statistics:\n- Total Screen Captures: {}\n- Unprocessed Captures: {}\n- Consolidated Long-Term Memories: {}\n- Composer Training Samples: {}\n",
                    stats.total_captures,
                    stats.unprocessed_captures,
                    stats.long_term_memories,
                    stats.training_samples
                );
                Ok(out)
            }

            unknown => Err(format!("Unknown memory tool '{}'", unknown)),
        }
    }
}

pub async fn run_mcp_stdio(db: Database) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let server = Arc::new(MemoryMcpServer::new(db));
    serve_stdio(stdin, stdout, server).await
}

pub async fn serve_stdio<R, W>(
    reader: R,
    mut writer: W,
    server: Arc<MemoryMcpServer>,
) -> Result<(), Box<dyn std::error::Error>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(resp) = handle_rpc_frame(&line, &server).await {
            let resp_bytes = serde_json::to_vec(&resp)?;
            writer.write_all(&resp_bytes).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

async fn handle_rpc_frame(line: &str, server: &MemoryMcpServer) -> Option<JsonRpcResponse> {
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

    let Some(id) = raw_val.get("id").cloned() else {
        return None;
    };

    let req: JsonRpcRequest = match serde_json::from_value(raw_val) {
        Ok(r) => r,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                id,
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
                    "name": "thunder-memory",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )),
        "ping" => Some(JsonRpcResponse::success(req.id, json!({}))),
        "tools/list" => {
            let result = McpListToolsResult {
                tools: MemoryMcpServer::tool_definitions(),
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
                        format!("Invalid tool call parameters: {e}"),
                    ));
                }
            };

            match server.call_tool(&call_params.name, call_params.arguments).await {
                Ok(text_content) => {
                    let result = McpCallToolResult::success_text(text_content);
                    Some(JsonRpcResponse::success(
                        req.id,
                        serde_json::to_value(result).unwrap_or_default(),
                    ))
                }
                Err(e) => {
                    let result = McpCallToolResult::error_text(e);
                    Some(JsonRpcResponse::success(
                        req.id,
                        serde_json::to_value(result).unwrap_or_default(),
                    ))
                }
            }
        }
        _ => Some(JsonRpcResponse::error(
            req.id,
            -32601,
            format!("Method not found: {}", req.method),
        )),
    }
}
