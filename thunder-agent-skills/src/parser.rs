use crate::error::SkillError;
use crate::types::Skill;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FrontmatterData {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    #[serde(default)]
    pub tags: serde_yaml::Value,
    #[serde(default)]
    pub triggers: serde_yaml::Value,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

pub struct SkillParser;

impl SkillParser {
    /// Parse skill from markdown content with optional YAML frontmatter.
    pub fn parse_markdown(content: &str, file_path: Option<&Path>) -> Result<Skill, SkillError> {
        let trimmed = content.trim();

        // 1. Try YAML frontmatter format (--- \n ... \n ---)
        if trimmed.starts_with("---") {
            if let Some(rest) = trimmed.strip_prefix("---") {
                if let Some(end_idx) = rest.find("\n---") {
                    let frontmatter_str = &rest[..end_idx];
                    let body = rest[end_idx + 4..].trim();

                    let fm: FrontmatterData =
                        serde_yaml::from_str(frontmatter_str).map_err(|e| {
                            SkillError::InvalidFrontmatter(format!(
                                "YAML frontmatter parse error: {e}"
                            ))
                        })?;

                    let name = fm.name.unwrap_or_else(|| {
                        file_path
                            .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
                            .unwrap_or("unnamed_skill")
                            .to_string()
                    });

                    let description = fm.description.unwrap_or_else(|| {
                        body.lines()
                            .next()
                            .unwrap_or("No description provided")
                            .to_string()
                    });

                    let tags = Self::extract_string_list(&fm.tags);
                    let triggers = Self::extract_string_list(&fm.triggers);

                    let mut skill = Skill::new(name, description, body)
                        .with_tags(tags)
                        .with_triggers(triggers);

                    if let Some(ver) = fm.version {
                        skill = skill.with_version(ver);
                    }
                    if let Some(auth) = fm.author {
                        skill = skill.with_author(auth);
                    }
                    if let Some(p) = file_path {
                        skill = skill.with_location(p.to_path_buf());
                    }

                    for (k, v) in fm.metadata {
                        skill = skill.with_metadata(k, v);
                    }

                    return Ok(skill);
                }
            }
        }

        // 2. Try XML tag format (<skill> ... </skill>)
        if trimmed.contains("<skill>") && trimmed.contains("</skill>") {
            return Self::parse_xml_format(trimmed, file_path);
        }

        // 3. Fallback: Pure Markdown format
        Self::parse_pure_markdown(trimmed, file_path)
    }

    /// Parse JSON format skill file.
    pub fn parse_json(content: &str, file_path: Option<&Path>) -> Result<Skill, SkillError> {
        let mut skill: Skill = serde_json::from_str(content)
            .map_err(|e| SkillError::ParseError(format!("JSON skill parse error: {e}")))?;

        if skill.location.is_none() {
            if let Some(p) = file_path {
                skill.location = Some(p.to_path_buf());
            }
        }

        Ok(skill)
    }

    /// Parse YAML format skill file.
    pub fn parse_yaml(content: &str, file_path: Option<&Path>) -> Result<Skill, SkillError> {
        let mut skill: Skill = serde_yaml::from_str(content)
            .map_err(|e| SkillError::ParseError(format!("YAML skill parse error: {e}")))?;

        if skill.location.is_none() {
            if let Some(p) = file_path {
                skill.location = Some(p.to_path_buf());
            }
        }

        Ok(skill)
    }

    /// Parse pure markdown without frontmatter.
    fn parse_pure_markdown(content: &str, file_path: Option<&Path>) -> Result<Skill, SkillError> {
        let mut lines = content.lines();
        let mut name = String::new();
        let mut description = String::new();
        let mut body_lines = Vec::new();
        let mut is_in_header = true;

        for line in lines.by_ref() {
            let line_trimmed = line.trim();
            if is_in_header {
                if line_trimmed.starts_with("# ") && name.is_empty() {
                    name = line_trimmed.trim_start_matches("# ").trim().to_string();
                    continue;
                }
                if line_trimmed.starts_with("> ") && description.is_empty() {
                    description = line_trimmed.trim_start_matches("> ").trim().to_string();
                    continue;
                }
                if !line_trimmed.is_empty() {
                    is_in_header = false;
                    body_lines.push(line);
                }
            } else {
                body_lines.push(line);
            }
        }

        if name.is_empty() {
            name = file_path
                .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
                .unwrap_or("unnamed_skill")
                .to_string();
        }

        if description.is_empty() {
            description = format!("Skill instructions for {}", name);
        }

        let body = if body_lines.is_empty() {
            content.to_string()
        } else {
            body_lines.join("\n")
        };

        let mut skill = Skill::new(name, description, body);
        if let Some(p) = file_path {
            skill = skill.with_location(p.to_path_buf());
        }

        Ok(skill)
    }

    /// Parse XML style snippet.
    fn parse_xml_format(content: &str, file_path: Option<&Path>) -> Result<Skill, SkillError> {
        let extract_tag = |tag: &str| -> Option<String> {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            if let Some(start) = content.find(&open) {
                if let Some(end) = content[start + open.len()..].find(&close) {
                    return Some(
                        content[start + open.len()..start + open.len() + end]
                            .trim()
                            .to_string(),
                    );
                }
            }
            None
        };

        let name = extract_tag("name").unwrap_or_else(|| {
            file_path
                .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
                .unwrap_or("unnamed_skill")
                .to_string()
        });

        let description =
            extract_tag("description").unwrap_or_else(|| format!("Skill instructions for {name}"));
        let instructions = extract_tag("instructions").unwrap_or_else(|| content.to_string());

        let mut skill = Skill::new(name, description, instructions);
        if let Some(p) = file_path {
            skill = skill.with_location(p.to_path_buf());
        }

        Ok(skill)
    }

    fn extract_string_list(val: &serde_yaml::Value) -> Vec<String> {
        match val {
            serde_yaml::Value::Sequence(seq) => seq
                .iter()
                .filter_map(|v| match v {
                    serde_yaml::Value::String(s) => Some(s.clone()),
                    serde_yaml::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect(),
            serde_yaml::Value::String(s) => s
                .lines()
                .map(|l| l.trim().trim_start_matches("- ").to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    }
}
