//! Declarative plugin loader and discovery for Thunder Agent.
//!
//! Assembles built-in tool plugins, dynamically connected MCP servers, and interceptors
//! based on layered `.thunder/config.json` declarations.

use crate::thunder_config::ThunderConfig;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use thunder_agent_policy::plugin::{InterceptorPlugin, ToolPlugin};
use tracing::{info, warn};

/// Collection of plugins loaded and filtered by the [`PluginLoader`].
pub struct LoadedPlugins {
    pub tool_plugins: Vec<Arc<dyn ToolPlugin>>,
    pub interceptors: Vec<Arc<dyn InterceptorPlugin>>,
}

/// Dynamic plugin loader reading configurations from `.thunder/config.json`.
pub struct PluginLoader;

impl PluginLoader {
    /// Load, filter, and bridge plugins according to the provided [`ThunderConfig`].
    pub async fn load_plugins(
        config: &ThunderConfig,
        workspace: Option<&Path>,
        register_builtins: bool,
    ) -> LoadedPlugins {
        Self::load_plugins_with_mcp(config, workspace, register_builtins, true).await
    }

    /// Load, filter, and bridge plugins, optionally delegating declarative MCP
    /// server management to an active [`crate::plugins::McpPlugin`] pool.
    pub async fn load_plugins_with_mcp(
        config: &ThunderConfig,
        workspace: Option<&Path>,
        register_builtins: bool,
        spawn_mcp_servers: bool,
    ) -> LoadedPlugins {
        let mut tool_plugins: Vec<Arc<dyn ToolPlugin>> = Vec::new();
        let interceptors: Vec<Arc<dyn InterceptorPlugin>> = Vec::new();

        let plugin_cfg = config.plugins();
        let disabled_set: HashSet<String> = plugin_cfg
            .and_then(|p| p.disabled.as_ref())
            .map(|list| list.iter().cloned().collect())
            .unwrap_or_default();

        let builtin_filter: Option<HashSet<String>> = plugin_cfg
            .and_then(|p| p.builtin.as_ref())
            .map(|list| list.iter().cloned().collect());

        // 1. Built-in coding plugins
        if register_builtins {
            let ws = workspace.map(|p| p.to_path_buf());
            let builtins = thunder_agent_pack_code::code_plugins(ws);
            for p in builtins {
                let id = p.id();
                // Check if explicitly whitelisted if builtin whitelist is defined
                if let Some(ref whitelist) = builtin_filter {
                    if !matches_plugin_id(whitelist, id) {
                        continue;
                    }
                }
                // Check if disabled
                if matches_plugin_id(&disabled_set, id) {
                    info!(plugin_id = %id, "Builtin plugin disabled via configuration");
                    continue;
                }
                tool_plugins.push(p);
            }
        }

        // 2. Declarative MCP Servers (skipped when a pooled McpPlugin is already active)
        #[cfg(feature = "mcp")]
        if spawn_mcp_servers {
            if let Some(plugin_sec) = plugin_cfg {
                if let Some(ref servers) = plugin_sec.mcp_servers {
                    let mcp_cfg = thunder_agent_mcp::config::McpConfig {
                        mcp_servers: servers.clone(),
                    };
                    match thunder_agent_mcp::manager::McpManager::from_config(mcp_cfg).await {
                        Ok(manager) => {
                            let mcp_plugins = manager.discover_all_tool_plugins().await;
                            for bridge in mcp_plugins {
                                let id = bridge.id();
                                if disabled_set.contains(id) {
                                    info!(plugin_id = %id, "MCP tool bridge disabled via configuration");
                                    continue;
                                }
                                tool_plugins.push(bridge);
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to initialize MCP servers from plugin configuration: {e}"
                            );
                        }
                    }
                }
            }
        }

        LoadedPlugins {
            tool_plugins,
            interceptors,
        }
    }
}

/// Check if a plugin id matches a set of configured IDs (including common aliases like `read` -> `read_file`).
pub fn matches_plugin_id(set: &HashSet<String>, id: &str) -> bool {
    if set.contains(id) {
        return true;
    }
    match id {
        "read_file" => set.contains("read"),
        "write_file" => set.contains("write"),
        "ls" => set.contains("list_dir"),
        "read" => set.contains("read_file"),
        "write" => set.contains("write_file"),
        "list_dir" => set.contains("ls"),
        _ => false,
    }
}
