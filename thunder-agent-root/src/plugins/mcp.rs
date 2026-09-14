#[cfg(feature = "mcp")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
use thunder_agent_loop::AgentRunResult;
pub use thunder_agent_mcp::prelude::*;
use tokio::sync::RwLock;
use tracing::info;

#[cfg(feature = "mcp")]
pub struct McpPlugin {
    manifest: PluginManifest,
    manager: McpManager,
    discovered_tools: Arc<RwLock<Vec<Arc<dyn AgentTool>>>>,
}

#[cfg(feature = "mcp")]
impl McpPlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            "mcp",
            "Model Context Protocol (MCP) Client",
            "Discovers, connects to, and dynamically registers tools from external MCP servers.",
            "0.1.0",
        )
        .with_capability(PluginCapability::McpProvider)
        .with_capability(PluginCapability::ToolProvider)
        .with_triggers(TriggerSpec::new(
            vec!["mcp", "model context protocol", "server tool", "external tool", "mcp server", "stdio tool"],
            "Connects to external MCP servers to discover and invoke external tool endpoints.",
        ));

        Self {
            manifest,
            manager: McpManager::new(),
            discovered_tools: Arc::new(RwLock::new(Vec::new())),
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
        Ok(Self {
            manifest: Self::new().manifest,
            manager,
            discovered_tools: Arc::new(RwLock::new(tools)),
        })
    }

    pub async fn from_config_file(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let manager = McpManager::from_config_file(path)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        let tools = manager.discover_all_tools().await;
        Ok(Self {
            manifest: Self::new().manifest,
            manager,
            discovered_tools: Arc::new(RwLock::new(tools)),
        })
    }

    pub async fn with_client(self, client: McpClient) -> Self {
        self.manager.register_client(client).await;
        let tools = self.manager.discover_all_tools().await;
        let mut guard = self.discovered_tools.write().await;
        *guard = tools;
        drop(guard);
        self
    }

    pub async fn with_stdio_server(self, name: impl Into<String>, config: &McpServerConfig) -> Result<Self, PluginError> {
        self.manager.add_stdio_server(name, config)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        let tools = self.manager.discover_all_tools().await;
        let mut guard = self.discovered_tools.write().await;
        *guard = tools;
        drop(guard);
        Ok(self)
    }

    pub fn manager(&self) -> &McpManager {
        &self.manager
    }

    pub async fn discovered_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let guard = self.discovered_tools.read().await;
        guard.clone()
    }
}

#[cfg(feature = "mcp")]
impl Default for McpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "mcp")]
#[async_trait]
impl ThunderPlugin for McpPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        if let Ok(guard) = self.discovered_tools.try_read() {
            guard.clone()
        } else {
            Vec::new()
        }
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        let servers = futures_util::FutureExt::now_or_never(self.manager.list_servers())?;
        if servers.is_empty() {
            return None;
        }

        let mut out = String::from("Connected MCP Tool Servers:\n");
        for server in &servers {
            out.push_str(&format!("- Server: `{}`\n", server));
        }

        if let Ok(guard) = self.discovered_tools.try_read() {
            if !guard.is_empty() {
                out.push_str("\nAvailable remote MCP tools:\n");
                for tool in guard.iter() {
                    let def = tool.definition();
                    out.push_str(&format!("- `{}`: {}\n", def.function.name, def.function.description));
                }
            }
        }

        Some(out)
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Auto-check for mcp_servers.json or claude_desktop_config.json in workspace if available
        if let Some(ws) = &ctx.workspace_dir {
            let mcp_cfg_path = ws.join("mcp_servers.json");
            let mcp_dot_json = ws.join(".mcp.json");
            if mcp_cfg_path.exists() {
                if let Ok(cfg) = McpConfig::from_file(&mcp_cfg_path).await {
                    let _ = self.manager.add_config(cfg).await;
                }
            } else if mcp_dot_json.exists() {
                if let Ok(cfg) = McpConfig::from_file(&mcp_dot_json).await {
                    let _ = self.manager.add_config(cfg).await;
                }
            }
        }

        let tools = self.manager.discover_all_tools().await;
        info!(tool_count = tools.len(), "McpPlugin updated discovered tools");
        let mut guard = self.discovered_tools.write().await;
        *guard = tools;

        Ok(())
    }

    async fn on_event(&self, _event: &ObservedEvent, _ctx: &PluginContext) {}

    async fn on_finish(&self, _result: &AgentRunResult, _ctx: &PluginContext) -> Result<(), PluginError> {
        Ok(())
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
                let _ = self.add_stdio_server(&name, &server_cfg).await;
            }
        }
    }
}
