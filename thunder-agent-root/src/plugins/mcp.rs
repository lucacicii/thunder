#[cfg(feature = "mcp")]
use crate::assembly::{Prepared, PromptSection, RunEnv};
#[cfg(feature = "mcp")]
use crate::error::PluginError;
use crate::thunder_config::ThunderConfig;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
pub use thunder_agent_mcp::prelude::*;
use thunder_agent_policy::AgentTool;
use tracing::info;

#[cfg(feature = "mcp")]
#[derive(Clone)]
pub struct McpPlugin {
    manager: McpManager,
    discovered_tools: Arc<RwLock<Vec<Arc<dyn AgentTool>>>>,
    connected_servers: Arc<RwLock<Vec<String>>>,
}

#[cfg(feature = "mcp")]
impl McpPlugin {
    pub const ID: &'static str = "mcp";

    pub fn new() -> Self {
        Self {
            manager: McpManager::new(),
            discovered_tools: Arc::new(RwLock::new(Vec::new())),
            connected_servers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn with_manager(mut self, manager: McpManager) -> Self {
        self.manager = manager;
        self
    }

    pub async fn from_config(config: McpConfig) -> Result<Self, PluginError> {
        let manager = McpManager::from_config(config)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        let tools = manager.discover_all_tools().await;
        let servers = manager.list_servers().await;
        Ok(Self {
            manager,
            discovered_tools: Arc::new(RwLock::new(tools)),
            connected_servers: Arc::new(RwLock::new(servers)),
        })
    }

    pub async fn from_config_file(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let manager = McpManager::from_config_file(path)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        let tools = manager.discover_all_tools().await;
        let servers = manager.list_servers().await;
        Ok(Self {
            manager,
            discovered_tools: Arc::new(RwLock::new(tools)),
            connected_servers: Arc::new(RwLock::new(servers)),
        })
    }

    pub async fn with_client(self, client: McpClient) -> Self {
        self.manager.register_client(client).await;
        let tools = self.manager.discover_all_tools().await;
        let servers = self.manager.list_servers().await;
        *self.discovered_tools.write() = tools;
        *self.connected_servers.write() = servers;
        self
    }

    pub async fn with_stdio_server(
        self,
        name: impl Into<String>,
        config: &McpServerConfig,
    ) -> Result<Self, PluginError> {
        self.manager
            .add_stdio_server(name, config)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        let tools = self.manager.discover_all_tools().await;
        let servers = self.manager.list_servers().await;
        *self.discovered_tools.write() = tools;
        *self.connected_servers.write() = servers;
        Ok(self)
    }

    pub fn manager(&self) -> &McpManager {
        &self.manager
    }

    pub async fn discovered_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        self.discovered_tools.read().clone()
    }
}

#[cfg(feature = "mcp")]
impl Default for McpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptSection for McpPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn render(&self) -> Option<String> {
        let mut servers = self.connected_servers.read().clone();
        if servers.is_empty() {
            servers = futures_util::FutureExt::now_or_never(self.manager.list_servers())
                .unwrap_or_default();
        }
        if servers.is_empty() {
            return None;
        }
        servers.sort();

        let mut out = String::from("Connected MCP Tool Servers:\n");
        for server in &servers {
            out.push_str(&format!("- Server: `{}`\n", server));
        }

        let guard = self.discovered_tools.read();
        if !guard.is_empty() {
            out.push_str("\nAvailable remote MCP tools:\n");
            for tool in guard.iter() {
                let def = tool.definition();
                out.push_str(&format!(
                    "- `{}`: {}\n",
                    def.function.name, def.function.description
                ));
            }
        }

        Some(out)
    }
}

impl Prepared for McpPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn prepare<'a>(
        &'a self,
        env: &'a RunEnv,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // Auto-check workspace MCP config. Project convention (`.thunder/mcp.json`)
            // takes precedence over the generic `.mcp.json` / `mcp_servers.json`.
            if let Some(ws) = &env.workspace {
                let project_mcp = ws.join(".thunder").join("mcp.json");
                let mcp_cfg_path = ws.join("mcp_servers.json");
                let mcp_dot_json = ws.join(".mcp.json");
                if project_mcp.exists() {
                    if let Ok(cfg) = McpConfig::from_file(&project_mcp).await {
                        let _ = self.manager.add_config(cfg).await;
                    }
                } else if mcp_cfg_path.exists() {
                    if let Ok(cfg) = McpConfig::from_file(&mcp_cfg_path).await {
                        let _ = self.manager.add_config(cfg).await;
                    }
                } else if mcp_dot_json.exists() {
                    if let Ok(cfg) = McpConfig::from_file(&mcp_dot_json).await {
                        let _ = self.manager.add_config(cfg).await;
                    }
                }
            }

            // Also merge declarative `plugins.mcpServers` from `.thunder/config.json`
            // so pooled `McpPlugin` instances manage them without per-turn re-spawning.
            let thunder_cfg = ThunderConfig::load(env.workspace.as_deref()).await;
            if let Some(servers) = thunder_cfg
                .plugins()
                .and_then(|p| p.mcp_servers.as_ref())
                .cloned()
            {
                self.manager
                    .add_config(McpConfig {
                        mcp_servers: servers,
                    })
                    .await;
            }

            let tools = self.manager.discover_all_tools().await;
            let servers = self.manager.list_servers().await;
            info!(
                tool_count = tools.len(),
                "McpPlugin updated discovered tools"
            );
            *self.discovered_tools.write() = tools;
            *self.connected_servers.write() = servers;

            Ok(())
        })
    }
}

impl McpPlugin {
    /// The tools discovered by the last `prepare`.
    pub fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        self.discovered_tools.read().clone()
    }
}
// Add helper method to McpManager in thunder_agent_mcp or extension
#[cfg(feature = "mcp")]
trait McpManagerExt {
    async fn add_config(&self, config: McpConfig);
}

#[cfg(feature = "mcp")]
impl McpManagerExt for McpManager {
    async fn add_config(&self, config: McpConfig) {
        for (name, server_cfg) in config.mcp_servers {
            if !server_cfg.disabled {
                if let Some(client) = self.get_client(&name).await {
                    if client.is_alive() {
                        continue;
                    }
                }
                let _ = self.add_stdio_server(&name, &server_cfg).await;
            }
        }
    }
}
