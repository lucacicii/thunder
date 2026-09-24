use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Messages sent from Rust to Node.js Sidecar
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostMessage {
    #[serde(rename = "init")]
    Init { plugin_dirs: Vec<PathBuf> },

    #[serde(rename = "reload")]
    Reload {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(skip_serializing_if = "Option::is_none")]
        plugin_dirs: Option<Vec<PathBuf>>,
    },

    #[serde(rename = "get_system_prompts")]
    GetSystemPrompts {
        request_id: String,
        context: serde_json::Value,
    },

    #[serde(rename = "dispatch_event")]
    DispatchEvent {
        event: serde_json::Value,
        context: serde_json::Value,
    },

    #[serde(rename = "execute_tool")]
    ExecuteTool {
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
        context: serde_json::Value,
    },

    #[serde(rename = "rpc_response")]
    RpcResponse {
        id: String,
        success: bool,
        data: Option<serde_json::Value>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub has_system_prompt: bool,
    pub has_on_event: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    pub plugin_id: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptContribution {
    pub plugin_id: String,
    pub plugin_name: String,
    pub prompt: String,
}

/// Messages received from Node.js Sidecar to Rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "init_ack")]
    InitAck { success: bool },

    #[serde(rename = "reload_ack")]
    ReloadAck {
        success: bool,
        plugin_id: Option<String>,
        error: Option<String>,
        kept_active: Option<bool>,
    },

    #[serde(rename = "manifest_synced")]
    ManifestSynced {
        plugins: Vec<PluginMeta>,
        tools: Vec<ToolMeta>,
    },

    #[serde(rename = "system_prompts_result")]
    SystemPromptsResult {
        request_id: String,
        prompts: Vec<PromptContribution>,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        call_id: String,
        success: bool,
        output: Option<String>,
        error: Option<String>,
    },

    #[serde(rename = "rpc_request")]
    RpcRequest {
        id: String,
        method: String,
        params: serde_json::Value,
    },
}
