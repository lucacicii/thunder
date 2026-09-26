use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin};
use std::collections::HashMap;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
use thunder_agent_loop::AgentRunResult;
use tracing::info;

#[derive(Clone, Default)]
pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn ThunderPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Register a plugin into the registry.
    pub fn register<P: ThunderPlugin + 'static>(&mut self, plugin: P) -> &mut Self {
        let manifest = plugin.manifest();
        let id = manifest.id.clone();
        info!(plugin_id = %id, plugin_name = %manifest.name, "Registering ThunderRoot plugin");
        self.plugins.insert(id, Arc::new(plugin));
        self
    }

    /// Register a plugin wrapped in Arc.
    pub fn register_arc(&mut self, plugin: Arc<dyn ThunderPlugin>) -> &mut Self {
        let manifest = plugin.manifest();
        let id = manifest.id.clone();
        info!(plugin_id = %id, plugin_name = %manifest.name, "Registering ThunderRoot plugin");
        self.plugins.insert(id, plugin);
        self
    }

    /// Unregister a plugin by ID.
    pub fn unregister(&mut self, id: &str) -> Option<Arc<dyn ThunderPlugin>> {
        self.plugins.remove(id)
    }

    /// Get a plugin by ID.
    pub fn get(&self, id: &str) -> Option<Arc<dyn ThunderPlugin>> {
        self.plugins.get(id).cloned()
    }

    /// List all registered plugins.
    pub fn list(&self) -> Vec<Arc<dyn ThunderPlugin>> {
        self.plugins.values().cloned().collect()
    }

    /// List all registered plugin manifests.
    pub fn list_manifests(&self) -> Vec<PluginManifest> {
        self.plugins
            .values()
            .map(|p| p.manifest().clone())
            .collect()
    }

    /// Find plugins matching a specific capability.
    pub fn find_by_capability(&self, capability: &PluginCapability) -> Vec<Arc<dyn ThunderPlugin>> {
        self.plugins
            .values()
            .filter(|p| p.manifest().capabilities.contains(capability))
            .cloned()
            .collect()
    }

    /// Build an active plugin set for a given subset of plugin IDs (always includes auto_always plugins).
    pub fn create_active_set(&self, selected_ids: &[String]) -> ActivePluginSet {
        let mut active = Vec::new();
        for plugin in self.plugins.values() {
            let manifest = plugin.manifest();
            if manifest.triggers.auto_always || selected_ids.contains(&manifest.id) {
                active.push(plugin.clone());
            }
        }
        ActivePluginSet { plugins: active }
    }
}

/// A resolved subset of plugins activated for a specific execution run.
#[derive(Clone)]
pub struct ActivePluginSet {
    plugins: Vec<Arc<dyn ThunderPlugin>>,
}

impl ActivePluginSet {
    pub fn new(plugins: Vec<Arc<dyn ThunderPlugin>>) -> Self {
        Self { plugins }
    }

    pub fn plugins(&self) -> &[Arc<dyn ThunderPlugin>] {
        &self.plugins
    }

    pub fn plugin_ids(&self) -> Vec<String> {
        self.plugins
            .iter()
            .map(|p| p.manifest().id.clone())
            .collect()
    }

    /// Collect all tools contributed by active plugins.
    pub fn collect_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let mut tools = Vec::new();
        for plugin in &self.plugins {
            tools.extend(plugin.tools());
        }
        tools
    }

    /// Build a combined system prompt incorporating contributions from all active plugins.
    pub fn build_combined_system_prompt(&self, base_prompt: Option<&str>) -> String {
        let mut prompt = base_prompt
            .unwrap_or("You are an autonomous engineering assistant powered by Thunder Agent.")
            .to_string();

        for plugin in &self.plugins {
            if let Some(contrib) = plugin.system_prompt_contribution() {
                prompt.push_str("\n\n");
                prompt.push_str(&format!(
                    "### [Plugin: {}]\n{}",
                    plugin.manifest().name,
                    contrib.trim()
                ));
            }
        }

        prompt
    }

    /// Dispatch lifecycle `on_init` to all active plugins.
    pub async fn dispatch_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        for plugin in &self.plugins {
            plugin.on_init(ctx).await?;
        }
        Ok(())
    }

    /// Dispatch lifecycle `on_event` to all active plugins.
    pub async fn dispatch_event(&self, event: &ObservedEvent, ctx: &PluginContext) {
        for plugin in &self.plugins {
            plugin.on_event(event, ctx).await;
        }
    }

    /// Dispatch lifecycle `on_finish` to all active plugins.
    pub async fn dispatch_finish(
        &self,
        result: &AgentRunResult,
        ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        for plugin in &self.plugins {
            plugin.on_finish(result, ctx).await?;
        }
        Ok(())
    }
}
