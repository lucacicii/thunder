use crate::registry::SkillRegistry;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};

/// Tool enabling the LLM to inspect and load the complete instruction set of a skill on demand.
pub struct LoadSkillTool {
    registry: SkillRegistry,
}

impl LoadSkillTool {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl AgentTool for LoadSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "load_skill",
            "Load and view the full instructions, prompt rules, and playbook of a specialized skill by name.",
            json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "The name or ID of the skill to load (e.g., 'code-review', 'debugging', 'frontend')"
                    }
                },
                "required": ["skill_name"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let name = args
            .get("skill_name")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("name").and_then(|v| v.as_str()))
            .ok_or_else(|| "Missing required parameter 'skill_name'".to_string())?;

        let skill = self
            .registry
            .get(name)
            .await
            .ok_or_else(|| format!("Skill '{name}' was not found in registry."))?;

        let mut out = format!("# Skill: {}\n", skill.name);
        out.push_str(&format!("**Description**: {}\n", skill.description));
        if !skill.tags.is_empty() {
            out.push_str(&format!("**Tags**: {}\n", skill.tags.join(", ")));
        }
        if !skill.triggers.is_empty() {
            out.push_str(&format!("**Triggers**: {}\n", skill.triggers.join(", ")));
        }
        if let Some(loc) = &skill.location {
            out.push_str(&format!("**Source Location**: {}\n", loc.display()));
        }
        out.push_str("\n## Instructions:\n\n");
        out.push_str(&skill.prompt_instructions);

        Ok(out)
    }
}

/// Tool listing all registered skills and their metadata.
pub struct ListSkillsTool {
    registry: SkillRegistry,
}

impl ListSkillsTool {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl AgentTool for ListSkillsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "list_skills",
            "List all available registered skills, descriptions, and trigger patterns in the system.",
            json!({
                "type": "object",
                "properties": {
                    "tag_filter": {
                        "type": "string",
                        "description": "Optional tag filter (e.g. 'git', 'review', 'frontend')"
                    }
                }
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let tag_filter = args.get("tag_filter").and_then(|v| v.as_str());
        let skills = self.registry.list().await;

        if skills.is_empty() {
            return Ok("No skills are currently registered in the registry.".to_string());
        }

        let filtered: Vec<_> = skills
            .into_iter()
            .filter(|s| {
                if let Some(filter) = tag_filter {
                    s.tags.iter().any(|t| t.eq_ignore_ascii_case(filter))
                } else {
                    true
                }
            })
            .collect();

        if filtered.is_empty() {
            return Ok(format!("No skills matched tag filter '{:?}'", tag_filter));
        }

        let mut out = format!("Found {} registered skill(s):\n\n", filtered.len());
        for s in filtered {
            out.push_str(&format!("- **{}**: {}\n", s.name, s.description));
            if !s.tags.is_empty() {
                out.push_str(&format!("  *Tags*: {}\n", s.tags.join(", ")));
            }
            if !s.triggers.is_empty() {
                out.push_str(&format!("  *Triggers*: {}\n", s.triggers.join(", ")));
            }
        }

        Ok(out)
    }
}

/// Tool for searching skills by free text query.
pub struct SearchSkillsTool {
    registry: SkillRegistry,
}

impl SearchSkillsTool {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl AgentTool for SearchSkillsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "search_skills",
            "Search for relevant skills matching a user intent, topic, or keyword query.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords, topic, or intent description to find matching skills"
                    }
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        let matches = if query.trim().is_empty() {
            self.registry.list().await
        } else {
            self.registry.match_prompt(query).await
        };

        if matches.is_empty() {
            return Ok(format!(
                "No matching skills found in registry (query: '{query}')."
            ));
        }

        let title = if query.trim().is_empty() {
            format!("Registered skills (total {}):\n\n", matches.len())
        } else {
            format!("Matched {} skill(s) for '{}':\n\n", matches.len(), query)
        };

        let mut out = title;
        for s in matches {
            out.push_str(&format!("- **{}**: {}\n", s.name, s.description));
        }

        Ok(out)
    }
}

/// Helper function to create all default skill tools.
pub fn create_skill_tools(registry: SkillRegistry) -> Vec<Arc<dyn AgentTool>> {
    vec![
        Arc::new(LoadSkillTool::new(registry.clone())),
        Arc::new(ListSkillsTool::new(registry.clone())),
        Arc::new(SearchSkillsTool::new(registry)),
    ]
}
