use crate::schema::ExecutionContext;
use crate::schema::ProgressKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// JSON-RPC 2.0 version constant.
pub const JSONRPC_VERSION: &str = "2.0";

/// Context envelope `_meta` injected into `tools/call` requests (§3.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEnvelopeMeta {
    pub session_id: String,
    pub workspace_cwd: PathBuf,
    pub timeout_ms: u64,
    /// Additional directories in scope for this call. Absent means "primary only",
    /// so a server that does not know the field keeps its old behaviour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_dirs: Vec<PathBuf>,
}

impl From<&ExecutionContext> for ContextEnvelopeMeta {
    fn from(ctx: &ExecutionContext) -> Self {
        Self {
            session_id: ctx.session_id.clone(),
            workspace_cwd: ctx.workspace_cwd.clone(),
            timeout_ms: ctx.timeout.as_millis() as u64,
            workspace_dirs: ctx.extra_workspace_dirs.clone(),
        }
    }
}

/// Parameters for `tools/call` with the Thunder `_meta` Context Envelope extension (§3.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<ContextEnvelopeMeta>,
}

/// Standard JSON-RPC 2.0 Request frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// Standard JSON-RPC 2.0 Notification frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Standard JSON-RPC 2.0 Error object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC 2.0 Response frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// MCP tool schema returned by `tools/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_object_schema")]
    pub input_schema: Value,
    /// Optional plugin-declared state-change semantics. Third-party MCP servers that
    /// do not send this field fall back to [`ProgressKind::Neutral`], which keeps
    /// their tools usable while leaving them out of the mutation signal.
    #[serde(default, skip_serializing_if = "ProgressKind::is_default")]
    pub progress: ProgressKind,
}

fn default_object_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

/// Result payload for `tools/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpListToolsResult {
    pub tools: Vec<McpToolInfo>,
}

/// Content item inside an MCP `tools/call` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContentBlock {
    Text { text: String },
}

/// Result payload for `tools/call`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallToolResult {
    pub content: Vec<McpContentBlock>,
    #[serde(default)]
    pub is_error: bool,
}

impl McpCallToolResult {
    pub fn success_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentBlock::Text { text: text.into() }],
            is_error: false,
        }
    }

    pub fn error_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![McpContentBlock::Text { text: text.into() }],
            is_error: true,
        }
    }

    pub fn combined_text(&self) -> String {
        self.content
            .iter()
            .map(|c| match c {
                McpContentBlock::Text { text } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod progress_wire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn progress_kind_survives_the_tools_list_wire() {
        let info = McpToolInfo {
            name: "write_file".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
            progress: ProgressKind::Mutating,
        };
        let wire = serde_json::to_value(&info).unwrap();
        assert_eq!(wire["progress"], json!("mutating"));
        let back: McpToolInfo = serde_json::from_value(wire).unwrap();
        assert_eq!(back.progress, ProgressKind::Mutating);
    }

    #[test]
    fn neutral_is_omitted_and_defaults_back() {
        let info = McpToolInfo {
            name: "bash".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
            progress: ProgressKind::Neutral,
        };
        let wire = serde_json::to_value(&info).unwrap();
        assert!(
            wire.get("progress").is_none(),
            "neutral must not be serialized so third-party servers stay byte-identical"
        );
        let back: McpToolInfo = serde_json::from_value(wire).unwrap();
        assert_eq!(back.progress, ProgressKind::Neutral);
    }

    #[test]
    fn third_party_servers_without_the_field_still_parse() {
        let wire = json!({
            "name": "mcp__git__commit",
            "description": "commit",
            "inputSchema": {"type": "object"}
        });
        let back: McpToolInfo = serde_json::from_value(wire).unwrap();
        assert_eq!(back.progress, ProgressKind::Neutral);
    }
}
