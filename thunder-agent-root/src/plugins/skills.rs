#[cfg(feature = "skills")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thunder_agent_loop::types::tool::AgentTool;
pub use thunder_agent_skills::prelude::*;

#[cfg(feature = "skills")]
pub struct SkillsPlugin {
    manifest: PluginManifest,
    registry: SkillRegistry,
    search_paths: Vec<PathBuf>,
}

#[cfg(feature = "skills")]
impl SkillsPlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            "skills",
            "Thunder Skill Parser & Registry",
            "Parses, manages and dynamically activates domain-specific agent skills and prompt instructions.",
            "0.1.0",
        )
        .with_capability(PluginCapability::SkillProvider)
        .with_capability(PluginCapability::ToolProvider)
        // Baseline plugin: always active (documented as "会话与技能常驻").
        // Keyword flicker on generic words like "spec" previously toggled this
        // plugin per message, destabilizing the cached request prefix.
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            registry: SkillRegistry::new(),
            search_paths: Vec::new(),
        }
    }

    pub fn with_registry(mut self, registry: SkillRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_skill(self, skill: Skill) -> Self {
        self.registry.register_sync(skill);
        self
    }

    pub fn with_search_path(mut self, path: PathBuf) -> Self {
        self.search_paths.push(path);
        self
    }

    pub async fn from_dir(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let registry = SkillRegistry::from_dir(path)
            .await
            .map_err(|e| PluginError::InitFailed(e.to_string()))?;
        Ok(Self::new().with_registry(registry))
    }

    pub async fn from_default_paths() -> Self {
        let registry = SkillRegistry::from_default_paths().await;
        Self::new().with_registry(registry)
    }

    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }
}

#[cfg(feature = "skills")]
impl Default for SkillsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

static DEFAULT_SKILLS_CACHE: std::sync::OnceLock<
    tokio::sync::RwLock<Option<(std::time::Instant, Vec<Skill>)>>,
> = std::sync::OnceLock::new();

async fn get_cached_default_skills() -> Vec<Skill> {
    let cache = DEFAULT_SKILLS_CACHE.get_or_init(|| tokio::sync::RwLock::new(None));
    {
        let r = cache.read().await;
        if let Some((timestamp, skills)) = r.as_ref() {
            if timestamp.elapsed() < std::time::Duration::from_secs(120) {
                return skills.clone();
            }
        }
    }
    let mut w = cache.write().await;
    if let Some((timestamp, skills)) = w.as_ref() {
        if timestamp.elapsed() < std::time::Duration::from_secs(120) {
            return skills.clone();
        }
    }
    let default_paths = SkillLoader::default_search_paths();
    let loaded = SkillLoader::load_search_paths(&default_paths).await;
    *w = Some((std::time::Instant::now(), loaded.clone()));
    loaded
}

#[cfg(feature = "skills")]
#[async_trait]
impl ThunderPlugin for SkillsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        create_skill_tools(self.registry.clone())
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        let skills = self.registry.list_sync();
        if skills.is_empty() {
            return None;
        }

        let mut out = String::from("Available specialized skills in registry:\n");
        for skill in &skills {
            let brief = skill.description.lines().next().unwrap_or("").trim();
            // Cap each catalog entry: some skill descriptions are single very long sentences
            // that would otherwise re-bloat the system prompt.
            let brief: String = if brief.chars().count() > 100 {
                let truncated: String = brief.chars().take(100).collect();
                format!("{}…", truncated.trim_end())
            } else {
                brief.to_string()
            };
            out.push_str(&format!("- **{}**: {}\n", skill.name, brief));
        }
        out.push_str("\nYou can use the `load_skill` tool to inspect full instructions for any of the above skills.\n");
        Some(out)
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // 1. Automatically load default system search paths (cached with 120s TTL)
        let loaded_defaults = get_cached_default_skills().await;
        for s in loaded_defaults {
            self.registry.register(s).await;
        }

        // 2. Automatically scan workspace directory for .agents/skills or skills/ if configured
        if let Some(ws) = &ctx.workspace_dir {
            let candidate1 = ws.join(".agents").join("skills");
            let candidate2 = ws.join(".pi").join("skills");
            let candidate3 = ws.join("skills");
            for candidate in [candidate1, candidate2, candidate3] {
                if candidate.is_dir() {
                    if let Ok(loaded) = SkillLoader::load_dir(&candidate).await {
                        for s in loaded {
                            self.registry.register(s).await;
                        }
                    }
                }
            }
        }

        // 3. Scan explicit custom search paths
        for path in &self.search_paths {
            if path.is_dir() {
                if let Ok(loaded) = SkillLoader::load_dir(path).await {
                    for s in loaded {
                        self.registry.register(s).await;
                    }
                }
            } else if path.is_file() {
                if let Ok(s) = SkillLoader::load_file(path).await {
                    self.registry.register(s).await;
                }
            }
        }

        Ok(())
    }
}
