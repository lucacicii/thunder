use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Represents a parsed, fully qualified agent skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    /// Unique identifier / name of the skill (e.g., "code-review", "frontend")
    pub name: String,

    /// Human & LLM readable summary of what the skill accomplishes
    pub description: String,

    /// Optional semver version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Optional author / maintainer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    /// Tags for categorization (e.g. ["git", "review", "frontend"])
    #[serde(default)]
    pub tags: Vec<String>,

    /// Activation triggers (keywords, regex patterns, or intent phrases)
    #[serde(default)]
    pub triggers: Vec<String>,

    /// The core instructions / markdown playbook injected into prompt context or loaded on demand
    pub prompt_instructions: String,

    /// Optional parameter schema for invocation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,

    /// Source file path where the skill was loaded from (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<PathBuf>,

    /// Extended custom metadata
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Skill {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        prompt_instructions: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: None,
            author: None,
            tags: Vec::new(),
            triggers: Vec::new(),
            prompt_instructions: prompt_instructions.into(),
            parameters: None,
            location: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.triggers.push(trigger.into());
        self
    }

    pub fn with_triggers(mut self, triggers: Vec<String>) -> Self {
        self.triggers = triggers;
        self
    }

    pub fn with_location(mut self, path: PathBuf) -> Self {
        self.location = Some(path);
        self
    }

    pub fn with_parameter_schema(mut self, schema: serde_json::Value) -> Self {
        self.parameters = Some(schema);
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, val: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), val);
        self
    }

    /// Check if this skill matches a given text query against its triggers, tags, name, or description.
    pub fn matches_query(&self, query: &str) -> bool {
        let q_lower = query.to_lowercase();
        let name_lower = self.name.to_lowercase();

        // 1. Direct name match
        if q_lower.contains(&name_lower) {
            return true;
        }

        // 2. Triggers match
        for trig in &self.triggers {
            let trig_lower = trig.to_lowercase();
            if q_lower.contains(&trig_lower) || trig_lower.contains(&q_lower) {
                return true;
            }
        }

        // 3. Tag match
        for tag in &self.tags {
            if q_lower.contains(&tag.to_lowercase()) {
                return true;
            }
        }

        false
    }

    /// Calculate match score (higher = stronger relevance).
    pub fn match_score(&self, query: &str) -> usize {
        let q_lower = query.to_lowercase();
        let mut score = 0;

        let name_lower = self.name.to_lowercase();
        if q_lower.contains(&name_lower) {
            score += 10;
        }

        for trig in &self.triggers {
            let trig_lower = trig.to_lowercase();
            if q_lower.contains(&trig_lower) {
                score += 5;
            }
        }

        for tag in &self.tags {
            if q_lower.contains(&tag.to_lowercase()) {
                score += 3;
            }
        }

        if self.description.to_lowercase().contains(&q_lower) {
            score += 2;
        }

        score
    }

    /// Convert to lightweight summary.
    pub fn summary(&self) -> SkillSummary {
        SkillSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            triggers: self.triggers.clone(),
            location: self.location.clone(),
        }
    }
}

/// Lightweight summary for skill cataloging and prompt index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub triggers: Vec<String>,
    pub location: Option<PathBuf>,
}

/// Attached skill handler for a session. Full playbook stays off-screen
/// until the model explicitly calls `load_skill`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillHandle {
    pub name: String,
    pub description: String,
    pub location: Option<PathBuf>,
}

impl SkillHandle {
    pub fn from_skill(skill: &Skill) -> Self {
        Self {
            name: skill.name.clone(),
            description: skill
                .description
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
            location: skill.location.clone(),
        }
    }

    pub fn from_summary(summary: &SkillSummary) -> Self {
        Self {
            name: summary.name.clone(),
            description: summary
                .description
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
            location: summary.location.clone(),
        }
    }

    pub fn from_picker(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            location: None,
        }
    }

    pub fn chip(&self) -> String {
        format!("skill:{}", self.name)
    }

    pub fn confirm_line(&self) -> String {
        format!(
            "Using skill **{}** — {}. Full playbook stays attached as a handler; the model can call `load_skill` if it needs the complete instructions.",
            self.name, self.description
        )
    }

    /// Compact system-prompt fragment. Does not include the full playbook body.
    pub fn system_prompt_fragment(&self) -> String {
        format!(
            "Active skill handler: **{}**.\n{}\nFollow this skill's workflow. Use the `load_skill` tool with name `{}` only if you need the full playbook.",
            self.name, self.description, self.name
        )
    }
}
