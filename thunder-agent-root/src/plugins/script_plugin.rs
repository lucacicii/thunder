#[cfg(feature = "script-plugin")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
pub use thunder_agent_plugin::TsScriptPluginEngine;
use tokio::sync::RwLock;
use tracing::info;

#[cfg(feature = "script-plugin")]
#[derive(Clone)]
pub struct ScriptPlugin {
    manifest: PluginManifest,
    workspace_dir: Option<PathBuf>,
    engine: Arc<RwLock<Option<TsScriptPluginEngine>>>,
    cached_tools: Arc<RwLock<Vec<Arc<dyn AgentTool>>>>,
    cached_prompt: Arc<RwLock<Option<String>>>,
}

#[cfg(feature = "script-plugin")]
impl ScriptPlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            "script_plugin",
            "TypeScript Single-File Plugins",
            "Zero-compile single-file TypeScript & JavaScript plugin runtime with hot-reload and sandbox integration.",
            "0.1.0",
        )
        .with_capability(PluginCapability::ToolProvider)
        .with_capability(PluginCapability::Custom("TypeScriptPlugin".to_string()))
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            workspace_dir: None,
            engine: Arc::new(RwLock::new(None)),
            cached_tools: Arc::new(RwLock::new(Vec::new())),
            cached_prompt: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_workspace(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    pub async fn reload(&self, path: Option<PathBuf>) {
        if let Some(engine) = self.engine.read().await.as_ref() {
            engine.reload(path).await;
            let tools = engine.list_tools().await;
            *self.cached_tools.write().await = tools;
            let prompt = engine.get_system_prompts().await;
            *self.cached_prompt.write().await = prompt;
        }
    }
}

#[cfg(feature = "script-plugin")]
impl Default for ScriptPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "script-plugin")]
#[async_trait]
impl ThunderPlugin for ScriptPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        futures_util::FutureExt::now_or_never(async { self.cached_tools.read().await.clone() })
            .unwrap_or_default()
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        futures_util::FutureExt::now_or_never(async { self.cached_prompt.read().await.clone() })
            .unwrap_or_default()
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let ws = ctx
            .workspace_dir
            .clone()
            .or_else(|| self.workspace_dir.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let mut lock = self.engine.write().await;
        if lock.is_none() {
            if let Some(engine) = TsScriptPluginEngine::create(ws).await {
                info!("Initialized TypeScript Script Plugin Engine");
                let tools = engine.list_tools().await;
                *self.cached_tools.write().await = tools;
                let prompt = engine.get_system_prompts().await;
                *self.cached_prompt.write().await = prompt;
                *lock = Some(engine);
            }
        }
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, ctx: &PluginContext) {
        if let Some(engine) = self.engine.read().await.as_ref() {
            engine
                .dispatch_event(event, &ctx.session_id, ctx.workspace_dir.as_ref())
                .await;
        }
    }
}
