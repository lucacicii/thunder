use crate::error::SkillError;
use crate::loader::SkillLoader;
use crate::types::{Skill, SkillSummary};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Clone, Default)]
pub struct SkillRegistry {
    skills: Arc<RwLock<HashMap<String, Skill>>>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load and construct a registry from a directory path.
    pub async fn from_dir(path: impl AsRef<Path>) -> Result<Self, SkillError> {
        let registry = Self::new();
        let loaded = SkillLoader::load_dir(path).await?;
        for skill in loaded {
            registry.register(skill).await;
        }
        Ok(registry)
    }

    /// Discover skills from default system paths.
    pub async fn from_default_paths() -> Self {
        let registry = Self::new();
        let paths = SkillLoader::default_search_paths();
        let loaded = SkillLoader::load_search_paths(&paths).await;
        for skill in loaded {
            registry.register(skill).await;
        }
        registry
    }

    /// Register a skill into the registry.
    pub async fn register(&self, skill: Skill) {
        let mut map = self.skills.write().await;
        info!(skill_name = %skill.name, "Registering skill in SkillRegistry");
        map.insert(skill.name.clone(), skill);
    }

    /// Synchronous register (creates task or lock for non-async setup).
    pub fn register_sync(&self, skill: Skill) {
        if let Ok(mut map) = self.skills.try_write() {
            map.insert(skill.name.clone(), skill);
        } else {
            let skills_clone = self.skills.clone();
            tokio::spawn(async move {
                let mut map = skills_clone.write().await;
                map.insert(skill.name.clone(), skill);
            });
        }
    }

    /// Unregister a skill by name.
    pub async fn unregister(&self, name: &str) -> Option<Skill> {
        let mut map = self.skills.write().await;
        map.remove(name)
    }

    /// Get a skill by name.
    pub async fn get(&self, name: &str) -> Option<Skill> {
        let map = self.skills.read().await;
        map.get(name).cloned()
    }

    /// Check if a skill exists.
    pub async fn contains(&self, name: &str) -> bool {
        let map = self.skills.read().await;
        map.contains_key(name)
    }

    /// Get total number of registered skills.
    pub async fn len(&self) -> usize {
        let map = self.skills.read().await;
        map.len()
    }

    /// Check if registry is empty.
    pub async fn is_empty(&self) -> bool {
        let map = self.skills.read().await;
        map.is_empty()
    }

    /// List all registered skills asynchronously.
    pub async fn list(&self) -> Vec<Skill> {
        let map = self.skills.read().await;
        map.values().cloned().collect()
    }

    /// List all registered skills synchronously (if lock is immediately available).
    pub fn list_sync(&self) -> Vec<Skill> {
        if let Ok(map) = self.skills.try_read() {
            map.values().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// List all registered skill summaries.
    pub async fn list_summaries(&self) -> Vec<SkillSummary> {
        let map = self.skills.read().await;
        map.values().map(|s| s.summary()).collect()
    }

    /// Match a user prompt against all registered skills and return ranked matches.
    pub async fn match_prompt(&self, prompt: &str) -> Vec<Skill> {
        let map = self.skills.read().await;
        let mut scored: Vec<(usize, Skill)> = map
            .values()
            .filter_map(|s| {
                let score = s.match_score(prompt);
                if score > 0 {
                    Some((score, s.clone()))
                } else {
                    None
                }
            })
            .collect();

        // Sort descending by match score
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.into_iter().map(|(_, s)| s).collect()
    }

    /// Build dynamic prompt instructions contribution for a subset of skills.
    pub fn build_prompt_contribution(skills: &[Skill]) -> Option<String> {
        if skills.is_empty() {
            return None;
        }

        let mut out = String::from(
            "The following specialized skills and playbooks are active and available:\n\n",
        );
        for skill in skills {
            out.push_str(&format!("## Skill: {}\n", skill.name));
            out.push_str(&format!("**Description**: {}\n", skill.description));
            if !skill.tags.is_empty() {
                out.push_str(&format!("**Tags**: {}\n", skill.tags.join(", ")));
            }
            out.push_str("\n### Instructions:\n");
            out.push_str(skill.prompt_instructions.trim());
            out.push_str("\n\n---\n\n");
        }

        Some(out.trim().to_string())
    }

    /// Build an XML-formatted skill catalog for LLM system prompt reference.
    pub async fn build_xml_catalog(&self) -> Option<String> {
        let map = self.skills.read().await;
        if map.is_empty() {
            return None;
        }

        let mut out = String::from("<available_skills>\n");
        for skill in map.values() {
            out.push_str("  <skill>\n");
            out.push_str(&format!("    <name>{}</name>\n", skill.name));
            out.push_str(&format!(
                "    <description>{}</description>\n",
                skill.description
            ));
            if let Some(loc) = &skill.location {
                out.push_str(&format!("    <location>{}</location>\n", loc.display()));
            }
            out.push_str("  </skill>\n");
        }
        out.push_str("</available_skills>");

        Some(out)
    }

    /// Render a human & LLM readable Markdown catalog of all registered skills.
    pub async fn render_catalog_markdown(&self) -> String {
        let skills = self.list().await;
        Self::format_skills_catalog_markdown(&skills)
    }

    /// Format a slice of skills into a Markdown catalog table.
    pub fn format_skills_catalog_markdown(skills: &[Skill]) -> String {
        if skills.is_empty() {
            return "No skills currently registered in the registry. Search paths (`~/.agents/skills`, `.agents/skills`, `.pi/skills`) appear empty.".to_string();
        }

        let mut out = format!(
            "### 📖 Registered Agent Skills Catalog (Total: {})\n\n",
            skills.len()
        );
        out.push_str("| Skill Name | Description | Triggers / Tags |\n");
        out.push_str("|---|---|---|\n");

        for s in skills {
            let tags = if !s.triggers.is_empty() {
                s.triggers.join(", ")
            } else {
                s.tags.join(", ")
            };
            let desc_brief = s.description.lines().next().unwrap_or("").trim();
            out.push_str(&format!(
                "| **`{}`** | {} | `{}` |\n",
                s.name, desc_brief, tags
            ));
        }

        out.push_str(
            "\n*Use `/skills load <name>` to view detailed playbook instructions for any skill.*",
        );
        out
    }

    /// Render the full markdown instructions and metadata of a single skill.
    pub async fn render_skill_markdown(&self, name: &str) -> Option<String> {
        let skill = self.get(name).await?;
        let mut out = format!("### 📖 Skill Playbook: `{}`\n", skill.name);
        out.push_str(&format!("**Description**: {}\n", skill.description));
        if !skill.tags.is_empty() {
            out.push_str(&format!("**Tags**: {}\n", skill.tags.join(", ")));
        }
        if !skill.triggers.is_empty() {
            out.push_str(&format!("**Triggers**: {}\n", skill.triggers.join(", ")));
        }
        if let Some(loc) = &skill.location {
            out.push_str(&format!("**Location**: `{}`\n", loc.display()));
        }
        out.push_str("\n---\n\n");
        out.push_str(&skill.prompt_instructions);
        Some(out)
    }
}
