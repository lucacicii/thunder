use crate::db::{AgentInteractionRow, Database, NewAgentInteraction};
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
    #[serde(rename = "inputSchema")]
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
                name: "memory_record".into(),
                description: Some("Record an explicit cognitive memory, persistent user preference, hard constraint / red-line, critical architecture decision, or complex bug root-cause solution into the system-wide long-term memory.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "enum": ["constraint", "user_preference", "architecture_decision", "bug_solution", "task_milestone"],
                            "description": "Category: constraint (red lines / prohibited actions), user_preference (habits/preferences), architecture_decision (system design choices), bug_solution (root cause & fix), task_milestone (key project milestone)."
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["global", "project"],
                            "description": "Scope: 'global' applies across all projects/directories; 'project' applies only to a specific workspace path."
                        },
                        "title": {
                            "type": "string",
                            "description": "Concise summary title (under 30 characters)."
                        },
                        "content": {
                            "type": "string",
                            "description": "Detailed explanation, rules, root cause analysis, or guidelines."
                        },
                        "entities": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Associated file names, technologies, packages, or symbols."
                        },
                        "project_path": {
                            "type": "string",
                            "description": "Absolute project workspace path (required when scope is 'project')."
                        },
                        "agent_name": {
                            "type": "string",
                            "description": "Name of the calling agent (e.g. pi, codex, claude, thunder)."
                        },
                        "importance": {
                            "type": "number",
                            "description": "Suggested importance score from 1.0 to 10.0 (default: 8.0)."
                        }
                    },
                    "required": ["category", "scope", "title", "content"]
                }),
            },
            McpToolInfo {
                name: "memory_get_preferences".into(),
                description: Some("Retrieve active user constraints, red lines, development preferences, and key architectural decisions across projects or for a specific project path. Call this before making significant changes or starting tasks.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "project_path": {
                            "type": "string",
                            "description": "Optional absolute path of the current project workspace. If omitted, returns global preferences only. If provided, returns global preferences plus project-specific rules."
                        }
                    }
                }),
            },
            McpToolInfo {
                name: "memory_query".into(),
                description: Some("Search long-term memories, agent-recorded rules/solutions, and desktop activity history using cognitive relevance, importance, and recency scoring.".into()),
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
            McpToolInfo {
                name: "memory_feedback".into(),
                description: Some("Reinforce or penalize retrieved memory utility based on agent evaluation or citation success.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "memory_type": {
                            "type": "string",
                            "enum": ["long_term", "agent_interaction"],
                            "description": "Type of memory: 'long_term' or 'agent_interaction'."
                        },
                        "memory_id": {
                            "type": "integer",
                            "description": "ID of the memory record to reinforce or penalize."
                        },
                        "utility_delta": {
                            "type": "number",
                            "description": "Positive value to reinforce (e.g. +1.0) or negative value to penalize (e.g. -0.5)."
                        }
                    },
                    "required": ["memory_type", "memory_id", "utility_delta"]
                }),
            },
            McpToolInfo {
                name: "memory_compact".into(),
                description: Some("Trigger aggressive compaction of ephemeral L1 captures and cold archiving of inactive L2 memories.".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "capture_retention_days": {
                            "type": "integer",
                            "description": "Ephemeral capture retention in days (default: 2)."
                        },
                        "memory_archive_days": {
                            "type": "integer",
                            "description": "Cold archive threshold in days (default: 30)."
                        }
                    }
                }),
            },
        ]
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        match name {
            "memory_record" => {
                let category = arguments
                    .get("category")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing required parameter 'category'".to_string())?;
                let scope = arguments
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("global");
                let title = arguments
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing required parameter 'title'".to_string())?;
                let content = arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "Missing required parameter 'content'".to_string())?;

                let entities: Vec<String> = arguments
                    .get("entities")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let project_path = arguments
                    .get("project_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let agent_name = arguments
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent")
                    .to_string();

                let suggested_importance = arguments
                    .get("importance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(8.0) as f32;

                let new_item = NewAgentInteraction {
                    agent_name,
                    scope: scope.to_string(),
                    project_path: project_path.clone(),
                    category: category.to_string(),
                    title: title.to_string(),
                    content: content.to_string(),
                    entities,
                    suggested_importance,
                    metadata: None,
                };

                let id = self
                    .db
                    .insert_agent_interaction(&new_item)
                    .map_err(|e| format!("Failed to record memory: {e}"))?;

                let scope_desc = match &project_path {
                    Some(p) => format!("project ({})", p),
                    None => "global".to_string(),
                };

                Ok(format!(
                    "✅ Successfully recorded memory #{} (`{}`)\n- Category: {}\n- Scope: {}\n- Title: {}\n- Status: active (previous matching rule superseded if existing)",
                    id, category, category, scope_desc, title
                ))
            }

            "memory_get_preferences" => {
                let project_path = arguments
                    .get("project_path")
                    .and_then(|v| v.as_str());

                let items = self
                    .db
                    .get_active_preferences(project_path)
                    .map_err(|e| format!("Failed to get preferences: {e}"))?;

                for item in &items {
                    let _ = self.db.track_memory_access("agent_interaction", item.id, 0.05);
                }

                Ok(format_preferences(&items, project_path))
            }

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

                let agent_records = self
                    .db
                    .search_agent_interactions(query, limit)
                    .map_err(|e| e.to_string())?;

                for r in &agent_records {
                    let _ = self.db.track_memory_access("agent_interaction", r.id, 0.1);
                }
                for item in &scored {
                    let _ = self.db.track_memory_access("long_term", item.memory.id, 0.1);
                }

                if scored.is_empty() && agent_records.is_empty() {
                    return Ok(format!("No long-term memories or agent records found matching '{}'.", query));
                }

                let mut out = format!("### Memory Search Results for `{}`:\n\n", query);

                if !agent_records.is_empty() {
                    out.push_str("#### 🤖 Agent Cognitive Records & Guidelines:\n");
                    for (i, r) in agent_records.iter().enumerate() {
                        let scope_badge = if r.scope == "global" {
                            "[GLOBAL]".to_string()
                        } else {
                            format!("[PROJECT: {}]", r.project_path.as_deref().unwrap_or("current"))
                        };
                        out.push_str(&format!(
                            "{}. **{} [{}] {}** ({})\n   {}\n",
                            i + 1,
                            scope_badge,
                            r.category,
                            r.title,
                            r.status,
                            r.content
                        ));
                        if !r.entities.is_empty() {
                            out.push_str(&format!("   *Entities: {}*\n", r.entities.join(", ")));
                        }
                    }
                    out.push('\n');
                }

                if !scored.is_empty() {
                    out.push_str("#### 🖥️ Desktop & Screen History Insights:\n");
                    for (i, item) in scored.iter().enumerate() {
                        let m = &item.memory;
                        out.push_str(&format!(
                            "{}. **[{}] {}** (Relevance: {:.2}, Importance: {:.1}/10)\n",
                            i + 1,
                            m.category,
                            m.title,
                            item.relevance_score,
                            m.importance_score
                        ));
                        out.push_str(&format!("   - **Summary**: {}\n", m.summary));
                        if !m.key_entities.is_empty() {
                            out.push_str(&format!("   - **Entities**: {}\n", m.key_entities.join(", ")));
                        }
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

                // Retrieval only: a superseded memory was folded into a
                // consolidated one, and an agent reading both would treat one
                // piece of work as two.
                let memories = self
                    .db
                    .list_recent_active_memories(limit)
                    .map_err(|e| e.to_string())?;

                let agent_records = self
                    .db
                    .list_recent_agent_interactions(limit)
                    .map_err(|e| e.to_string())?;

                if memories.is_empty() && agent_records.is_empty() {
                    return Ok("No memories recorded yet.".into());
                }

                let mut out = String::from("### Recent Memories & Guidelines:\n\n");

                if !agent_records.is_empty() {
                    out.push_str("#### 🤖 Agent Cognitive Records:\n");
                    for (i, r) in agent_records.iter().enumerate() {
                        let scope_badge = if r.scope == "global" {
                            "[GLOBAL]".to_string()
                        } else {
                            format!("[PROJECT: {}]", r.project_path.as_deref().unwrap_or("current"))
                        };
                        out.push_str(&format!(
                            "{}. **{} [{}] {}**\n   {}\n",
                            i + 1,
                            scope_badge,
                            r.category,
                            r.title,
                            r.content
                        ));
                    }
                    out.push('\n');
                }

                if !memories.is_empty() {
                    out.push_str("#### 🖥️ Desktop Screen Insights:\n");
                    for (i, m) in memories.iter().enumerate() {
                        out.push_str(&format!(
                            "{}. **[{}] {}** (Score: {:.1}/10)\n",
                            i + 1,
                            m.category,
                            m.title,
                            m.importance_score
                        ));
                        out.push_str(&format!("   - {}\n", m.summary));
                        if !m.key_entities.is_empty() {
                            out.push_str(&format!("   - **Entities**: {}\n", m.key_entities.join(", ")));
                        }
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
                        format!("{}...", crate::safe_truncate_str(&c.ocr_text, 1000))
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
                    "### Thunder Memory Statistics:\n- Total Screen Captures: {}\n- Unprocessed Captures: {}\n- Consolidated Long-Term Memories: {}\n- Agent Interactions / Guidelines: {}\n- Composer Training Samples: {}\n- Cognitive Noise Suppression Hits: {}\n",
                    stats.total_captures,
                    stats.unprocessed_captures,
                    stats.long_term_memories,
                    stats.agent_interactions,
                    stats.training_samples,
                    stats.suppression_mask_hits
                );
                Ok(out)
            }

            "memory_feedback" => {
                let memory_type = arguments
                    .get("memory_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent_interaction");
                let memory_id = arguments
                    .get("memory_id")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| "Missing required parameter 'memory_id'".to_string())?;
                let utility_delta = arguments
                    .get("utility_delta")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32;

                self.db
                    .track_memory_access(memory_type, memory_id, utility_delta)
                    .map_err(|e| format!("Failed to record feedback: {e}"))?;

                Ok(format!(
                    "✅ Recorded feedback for {} #{}: utility delta {:+.2}",
                    memory_type, memory_id, utility_delta
                ))
            }

            "memory_compact" => {
                let capture_days = arguments
                    .get("capture_retention_days")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2) as u32;
                let archive_days = arguments
                    .get("memory_archive_days")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30) as u32;

                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let archive_path =
                    std::path::PathBuf::from(home).join(".thunder").join("memory_archive.db");

                let res = self
                    .db
                    .compact_and_archive(&archive_path, capture_days, archive_days)
                    .map_err(|e| format!("Compaction failed: {e}"))?;

                Ok(format!(
                    "✅ Compaction complete:\n- Pruned L1 captures: {}\n- Cold archived L2 memories: {}\n- Active DB size: {:.1} KB -> {:.1} KB",
                    res.pruned_captures,
                    res.archived_memories,
                    res.active_db_bytes_before as f64 / 1024.0,
                    res.active_db_bytes_after as f64 / 1024.0
                ))
            }

            unknown => Err(format!("Unknown memory tool '{}'", unknown)),
        }
    }
}

fn format_preferences(items: &[AgentInteractionRow], project_path: Option<&str>) -> String {
    if items.is_empty() {
        return match project_path {
            Some(p) => format!(
                "No active constraints or preferences recorded for workspace '{}' or globally.\nUse 'memory_record' to persist rules.",
                p
            ),
            None => "No active global constraints or preferences recorded.\nUse 'memory_record' to persist rules.".to_string(),
        };
    }

    let mut out = String::new();
    let constraints: Vec<&AgentInteractionRow> = items
        .iter()
        .filter(|i| i.category == "constraint")
        .collect();
    let preferences: Vec<&AgentInteractionRow> = items
        .iter()
        .filter(|i| i.category == "user_preference")
        .collect();
    let architecture: Vec<&AgentInteractionRow> = items
        .iter()
        .filter(|i| i.category == "architecture_decision")
        .collect();
    let others: Vec<&AgentInteractionRow> = items
        .iter()
        .filter(|i| {
            !matches!(
                i.category.as_str(),
                "constraint" | "user_preference" | "architecture_decision"
            )
        })
        .collect();

    if !constraints.is_empty() {
        out.push_str("### 🛑 Active Constraints & Red Lines:\n\n");
        for c in constraints {
            let scope_badge = if c.scope == "global" {
                "[GLOBAL]".to_string()
            } else {
                format!("[PROJECT: {}]", c.project_path.as_deref().unwrap_or("current"))
            };
            out.push_str(&format!("- **{} {}**\n  {}\n", scope_badge, c.title, c.content));
            if !c.entities.is_empty() {
                out.push_str(&format!("  *Entities: {}*\n", c.entities.join(", ")));
            }
        }
        out.push('\n');
    }

    if !preferences.is_empty() {
        out.push_str("### 💡 User Preferences & Coding Habits:\n\n");
        for p in preferences {
            let scope_badge = if p.scope == "global" {
                "[GLOBAL]".to_string()
            } else {
                format!("[PROJECT: {}]", p.project_path.as_deref().unwrap_or("current"))
            };
            out.push_str(&format!("- **{} {}**\n  {}\n", scope_badge, p.title, p.content));
            if !p.entities.is_empty() {
                out.push_str(&format!("  *Entities: {}*\n", p.entities.join(", ")));
            }
        }
        out.push('\n');
    }

    if !architecture.is_empty() {
        out.push_str("### 📐 Key Architecture Decisions:\n\n");
        for a in architecture {
            let scope_badge = if a.scope == "global" {
                "[GLOBAL]".to_string()
            } else {
                format!("[PROJECT: {}]", a.project_path.as_deref().unwrap_or("current"))
            };
            out.push_str(&format!("- **{} {}**\n  {}\n", scope_badge, a.title, a.content));
            if !a.entities.is_empty() {
                out.push_str(&format!("  *Entities: {}*\n", a.entities.join(", ")));
            }
        }
        out.push('\n');
    }

    if !others.is_empty() {
        out.push_str("### 📌 Other Active Guidelines:\n\n");
        for o in others {
            out.push_str(&format!("- **[{}] {}**\n  {}\n", o.category, o.title, o.content));
        }
        out.push('\n');
    }

    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_tools_list() {
        let tools = MemoryMcpServer::tool_definitions();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        assert!(tool_names.contains(&"memory_record".into()));
        assert!(tool_names.contains(&"memory_get_preferences".into()));
        assert!(tool_names.contains(&"memory_query".into()));
        assert!(tool_names.contains(&"memory_recent".into()));
        assert!(tool_names.contains(&"memory_current_screen".into()));
        assert!(tool_names.contains(&"memory_stats".into()));
        assert!(tool_names.contains(&"memory_feedback".into()));
        assert!(tool_names.contains(&"memory_compact".into()));
    }

    #[tokio::test]
    async fn test_mcp_record_and_get_preferences() {
        let db = Database::in_memory().unwrap();
        let server = MemoryMcpServer::new(db);

        // 1. Record global user preference
        let res1 = server
            .call_tool(
                "memory_record",
                json!({
                    "category": "user_preference",
                    "scope": "global",
                    "title": "Concise code edits",
                    "content": "Keep edits small and avoid large unchanged blocks",
                    "entities": ["edit", "code"]
                }),
            )
            .await
            .unwrap();
        assert!(res1.contains("Successfully recorded memory"));

        // 2. Record project-specific constraint
        let res2 = server
            .call_tool(
                "memory_record",
                json!({
                    "category": "constraint",
                    "scope": "project",
                    "project_path": "/Users/lucas/wz/project-x",
                    "title": "No Lombok",
                    "content": "Never use lombok annotations in project-x",
                    "entities": ["lombok", "java"]
                }),
            )
            .await
            .unwrap();
        assert!(res2.contains("Successfully recorded memory"));

        // 3. Query preferences without project_path -> only global
        let global_prefs = server
            .call_tool("memory_get_preferences", json!({}))
            .await
            .unwrap();
        assert!(global_prefs.contains("Concise code edits"));
        assert!(!global_prefs.contains("No Lombok"));

        // 4. Query preferences with project_path -> global + project
        let project_prefs = server
            .call_tool(
                "memory_get_preferences",
                json!({
                    "project_path": "/Users/lucas/wz/project-x"
                }),
            )
            .await
            .unwrap();
        assert!(project_prefs.contains("Concise code edits"));
        assert!(project_prefs.contains("No Lombok"));

        // 5. Query via memory_query -> finds agent records
        let query_res = server
            .call_tool(
                "memory_query",
                json!({
                    "query": "Lombok"
                }),
            )
            .await
            .unwrap();
        assert!(query_res.contains("Agent Cognitive Records"));
        assert!(query_res.contains("No Lombok"));

        // 6. Test memory_feedback
        let feedback_res = server
            .call_tool(
                "memory_feedback",
                json!({
                    "memory_type": "agent_interaction",
                    "memory_id": 2,
                    "utility_delta": 0.5
                }),
            )
            .await
            .unwrap();
        assert!(feedback_res.contains("utility delta +0.50"));

        // 7. Test memory_stats
        let stats_res = server
            .call_tool("memory_stats", json!({}))
            .await
            .unwrap();
        assert!(stats_res.contains("Thunder Memory Statistics"));
        assert!(stats_res.contains("Cognitive Noise Suppression Hits"));

        // 8. Test memory_compact
        let compact_res = server
            .call_tool(
                "memory_compact",
                json!({
                    "capture_retention_days": 2,
                    "memory_archive_days": 30
                }),
            )
            .await
            .unwrap();
        assert!(compact_res.contains("Compaction complete"));
    }

    #[tokio::test]
    async fn test_rpc_frame_handling() {
        let db = Database::in_memory().unwrap();
        let server = MemoryMcpServer::new(db);

        // Ping
        let ping_req = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = handle_rpc_frame(ping_req, &server).await.unwrap();
        assert_eq!(resp.id, json!(1));

        // Tools list
        let list_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = handle_rpc_frame(list_req, &server).await.unwrap();
        assert_eq!(resp.id, json!(2));
        assert!(resp.result.unwrap().get("tools").unwrap().as_array().unwrap().len() >= 8);
    }
}

