#[cfg(feature = "script-plugin")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
pub use thunder_agent_plugin::{run_registry, RunRegistry, TsScriptPluginEngine};
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Clone)]
pub struct ScriptPlugin {
    manifest: PluginManifest,
    workspace_dir: Option<PathBuf>,
    /// Per-run services handed to the sidecar, keyed by [`PluginContext::route`].
    ///
    /// Deliberately a map and not three process-wide slots. One Node process
    /// serves every concurrent run, so a single tier / UI / invoker would let the
    /// last-initialised run decide what all the others may do — and since the
    /// invoker is a *pipeline*, that includes its workspace root and path jail.
    runs: RunRegistry,
    engine: Arc<RwLock<Option<TsScriptPluginEngine>>>,
    cached_tools: Arc<RwLock<Vec<Arc<dyn AgentTool>>>>,
    cached_prompt: Arc<RwLock<Option<String>>>,
    scanned_workspaces: Arc<RwLock<std::collections::HashSet<PathBuf>>>,
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
            runs: run_registry(),
            engine: Arc::new(RwLock::new(None)),
            cached_tools: Arc::new(RwLock::new(Vec::new())),
            cached_prompt: Arc::new(RwLock::new(None)),
            scanned_workspaces: Arc::new(RwLock::new(std::collections::HashSet::new())),
        }
    }

    pub fn with_workspace(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    /// The per-run services registry, for hosts that want to inspect it.
    pub fn runs(&self) -> RunRegistry {
        Arc::clone(&self.runs)
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

/// The route a run should be registered under.
///
/// Falls back to a sentinel rather than `None` so an unidentified run still gets
/// a *named* slot: a route that exists and is read-only is far better than one
/// that is missing, because missing means every call is refused.
fn route_of(ctx: &PluginContext) -> String {
    ctx.route
        .clone()
        .unwrap_or_else(|| format!("unrouted:{}", ctx.session_id))
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
        let route = route_of(ctx);

        let mut lock = self.engine.write().await;
        if lock.is_none() {
            if let Some(engine) =
                TsScriptPluginEngine::create(ws.clone(), Arc::clone(&self.runs)).await
            {
                info!("Initialized TypeScript Script Plugin Engine");
                let tools = engine.list_tools().await;
                *self.cached_tools.write().await = tools;
                let prompt = engine.get_system_prompts().await;
                *self.cached_prompt.write().await = prompt;
                self.scanned_workspaces.write().await.insert(ws.clone());
                *lock = Some(engine);
            }
        } else if !self.scanned_workspaces.read().await.contains(&ws) {
            // Dynamic workspace discovery: rescan if this workspace's .arp/plugins exists
            let ws_plugin_dir = ws.join(".arp").join("plugins");
            if ws_plugin_dir.exists() {
                if let Some(engine) = lock.as_ref() {
                    engine.add_plugin_dir(ws_plugin_dir).await;
                    let tools = engine.list_tools().await;
                    *self.cached_tools.write().await = tools;
                    let prompt = engine.get_system_prompts().await;
                    *self.cached_prompt.write().await = prompt;
                }
            }
            self.scanned_workspaces.write().await.insert(ws.clone());
        }

        // Phase one of two: identity and policy, both per-run. Registered on
        // *every* init, not just the boot above, so a later run with a tighter
        // role actually narrows what its own plugins may do.
        if lock.is_some() {
            // The policy, not a bare tier: a plugin's `ctx.exec` is judged by the
            // same rules as the model's calls, modes and remembered rules
            // included.
            self.runs
                .write()
                .await
                .begin_run(&route, ws, ctx.policy(), Some(ctx.ui()))
                .await;
        }
        Ok(())
    }

    async fn on_run_ready(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Phase two: the pipeline, which only exists once every tool is
        // registered. Until it lands, `ctx.callTool` is refused rather than
        // routed somewhere plausible.
        let Some(invoker) = ctx.tools.0.read().await.clone() else {
            warn!(route = %route_of(ctx), "No tool invoker for this run; ctx.callTool will be refused");
            return Ok(());
        };
        self.runs
            .write()
            .await
            .set_tools(&route_of(ctx), invoker)
            .await;
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, ctx: &PluginContext) {
        if let Some(engine) = self.engine.read().await.as_ref() {
            engine
                .dispatch_event(
                    event,
                    &ctx.session_id,
                    ctx.route.as_deref(),
                    ctx.workspace_dir.as_ref(),
                )
                .await;
        }
    }

    async fn on_finish(
        &self,
        _result: &thunder_agent_loop::AgentRunResult,
        ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        // Drop the run's services so a later run reusing the id does not inherit
        // its grants, and so the registry stays bounded.
        self.runs.write().await.end_run(&route_of(ctx)).await;
        Ok(())
    }
}
