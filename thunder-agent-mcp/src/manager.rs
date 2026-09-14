use crate::client::McpClient;
use crate::config::{McpConfig, McpServerConfig};
use crate::error::McpError;
use crate::tool_bridge::McpToolBridge;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use thunder_agent_loop::types::tool::AgentTool;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Clone, Default)]
pub struct McpManager {
    clients: Arc<RwLock<HashMap<String, Arc<McpClient>>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Construct a manager from a parsed `McpConfig` (connects to non-disabled servers).
    pub async fn from_config(config: McpConfig) -> Result<Self, McpError> {
        let manager = Self::new();
        for (name, server_cfg) in config.mcp_servers {
            if server_cfg.disabled {
                info!(server = %name, "Skipping disabled MCP server");
                continue;
            }

            match manager.add_stdio_server(&name, &server_cfg).await {
                Ok(_) => {
                    info!(server = %name, "Successfully connected MCP server");
                }
                Err(e) => {
                    warn!(server = %name, "Failed to connect MCP server during manager initialization: {e}");
                }
            }
        }
        Ok(manager)
    }

    /// Load and connect servers from an MCP configuration file.
    pub async fn from_config_file(path: impl AsRef<Path>) -> Result<Self, McpError> {
        let config = McpConfig::from_file(path).await?;
        Self::from_config(config).await
    }

    /// Add a stdio-based server connection.
    pub async fn add_stdio_server(&self, name: impl Into<String>, config: &McpServerConfig) -> Result<Arc<McpClient>, McpError> {
        let s_name = name.into();
        let client = McpClient::connect_stdio(&s_name, config).await?;
        let client_arc = Arc::new(client);

        let mut map = self.clients.write().await;
        map.insert(s_name, client_arc.clone());

        Ok(client_arc)
    }

    /// Add an already connected McpClient.
    pub async fn register_client(&self, client: McpClient) {
        let name = client.server_name().to_string();
        let mut map = self.clients.write().await;
        map.insert(name, Arc::new(client));
    }

    /// Get a client by server name.
    pub async fn get_client(&self, name: &str) -> Option<Arc<McpClient>> {
        let map = self.clients.read().await;
        map.get(name).cloned()
    }

    /// List all connected server names.
    pub async fn list_servers(&self) -> Vec<String> {
        let map = self.clients.read().await;
        map.keys().cloned().collect()
    }

    /// Discover and bridge all tools exposed across all connected MCP servers.
    pub async fn discover_all_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let mut all_tools: Vec<Arc<dyn AgentTool>> = Vec::new();
        let map = self.clients.read().await;

        for (name, client) in map.iter() {
            match client.list_tools().await {
                Ok(tools) => {
                    info!(server = %name, count = tools.len(), "Discovered MCP tools from server");
                    for tool in tools {
                        let bridge = McpToolBridge::new(name, tool, client.clone());
                        all_tools.push(Arc::new(bridge));
                    }
                }
                Err(e) => {
                    warn!(server = %name, "Failed to list tools from MCP server: {e}");
                }
            }
        }

        all_tools
    }

    /// Render all discovered remote MCP tools into a Markdown table.
    pub async fn render_tools_markdown(&self) -> String {
        let tools = self.discover_all_tools().await;
        if tools.is_empty() {
            return "No remote MCP tools discovered from connected servers.".to_string();
        }

        let mut out = format!("### 🔧 Discovered MCP Remote Tools (Total: {})\n\n", tools.len());
        out.push_str("| Tool Name | Description |\n");
        out.push_str("|---|---|\n");

        for tool in tools {
            let def = tool.definition();
            let desc_brief = def.function.description.lines().next().unwrap_or("").trim();
            out.push_str(&format!("| **`{}`** | {} |\n", def.function.name, desc_brief));
        }

        out
    }

    /// Close all connected client transports.
    pub async fn close_all(&self) {
        let map = self.clients.read().await;
        for (name, client) in map.iter() {
            let _ = client.close().await;
            info!(server = %name, "Closed MCP server connection");
        }
    }
}
