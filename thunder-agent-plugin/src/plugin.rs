use crate::process::{SidecarConfig, SidecarManager};
use crate::tool_bridge::TsToolBridge;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
use tracing::warn;

pub struct TsScriptPluginEngine {
    sidecar: Arc<SidecarManager>,
}

impl TsScriptPluginEngine {
    pub async fn create(workspace_dir: PathBuf) -> Option<Self> {
        if !SidecarManager::is_node_available().await {
            warn!("Node.js runtime not found. TypeScript plugins will be disabled.");
            return None;
        }

        let mut plugin_dirs = Vec::new();

        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let global_dir = home.join(".thunder").join("plugins");
            let _ = tokio::fs::create_dir_all(&global_dir).await;
            plugin_dirs.push(global_dir);
        }

        let ws_plugin_dir = workspace_dir.join(".arp").join("plugins");
        let _ = tokio::fs::create_dir_all(&ws_plugin_dir).await;
        plugin_dirs.push(ws_plugin_dir);

        let runner_path = Self::locate_runner_script(&workspace_dir);
        if !runner_path.exists() {
            warn!(path = ?runner_path, "Plugin host runner script not found. TS plugins disabled.");
            return None;
        }

        let config = SidecarConfig {
            runner_path,
            plugin_dirs,
            workspace_dir: workspace_dir.clone(),
        };

        let sidecar = SidecarManager::new(config);
        if let Err(err) = sidecar.start().await {
            warn!(error = %err, "Failed to initialize TypeScript plugin sidecar");
            return None;
        }

        // Wait up to 500ms for initial manifest sync
        for _ in 0..10 {
            if !sidecar.list_plugins().await.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        Some(Self { sidecar })
    }

    fn locate_runner_script(ws: &PathBuf) -> PathBuf {
        let candidates = [
            ws.join("thunder-agent-plugin").join("runner").join("host.mjs"),
            PathBuf::from("/Users/lucas/wz/GitHub/thunder/thunder-agent-plugin/runner/host.mjs"),
            PathBuf::from("./thunder-agent-plugin/runner/host.mjs"),
        ];

        for c in &candidates {
            if c.exists() {
                return c.clone();
            }
        }

        candidates[0].clone()
    }

    pub fn sidecar(&self) -> Arc<SidecarManager> {
        Arc::clone(&self.sidecar)
    }

    pub async fn reload(&self, path: Option<PathBuf>) {
        self.sidecar.reload(path).await;
    }

    pub async fn list_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let tools_meta = self.sidecar.list_tools().await;
        let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
        for meta in tools_meta {
            tools.push(Arc::new(TsToolBridge::new(meta, Arc::clone(&self.sidecar))));
        }
        tools
    }

    pub async fn get_system_prompts(&self) -> Option<String> {
        let prompts = self.sidecar.get_system_prompts(serde_json::json!({})).await;
        if prompts.is_empty() {
            None
        } else {
            let combined = prompts
                .into_iter()
                .map(|p| format!("[Plugin: {}] {}", p.plugin_name, p.prompt))
                .collect::<Vec<_>>()
                .join("\n\n");
            Some(combined)
        }
    }

    pub async fn dispatch_event(&self, event: &ObservedEvent, session_id: &str, workspace_dir: Option<&PathBuf>) {
        if let Ok(event_json) = serde_json::to_value(event) {
            let ctx_json = serde_json::json!({
                "sessionId": session_id,
                "workspaceDir": workspace_dir.map(|p| p.to_string_lossy().to_string()),
            });
            self.sidecar.dispatch_event(event_json, ctx_json).await;
        }
    }
}
