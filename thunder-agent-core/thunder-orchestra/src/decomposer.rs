use crate::config::UnitSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubTask {
    pub id: String,
    pub role: String,
    pub title: String,
    pub prompt: String,
}

pub trait TaskDecomposer: Send + Sync {
    fn decompose(&self, prompt: &str, units: &[UnitSpec]) -> Vec<SubTask>;
}

/// Deterministic, structural task decomposer.
/// Inspects numbered items (1. 2. 3.), bullet points (- / *), or sections to partition
/// work across available specialist units. If monolithic, partitions the task by available unit roles.
#[derive(Default, Clone)]
pub struct HeuristicDecomposer;

impl TaskDecomposer for HeuristicDecomposer {
    fn decompose(&self, prompt: &str, units: &[UnitSpec]) -> Vec<SubTask> {
        if units.is_empty() {
            return vec![SubTask {
                id: "subtask_1".to_string(),
                role: "assistant".to_string(),
                title: "Primary Task".to_string(),
                prompt: prompt.to_string(),
            }];
        }

        // Try extracting structured items (e.g. lines starting with 1., 2., or -, *)
        let mut items = Vec::new();
        for line in prompt.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Check numbered list: e.g. "1. " or "2. "
            if let Some(first_char) = trimmed.chars().next() {
                if first_char.is_ascii_digit() {
                    if let Some(pos) = trimmed.find(". ").or_else(|| trimmed.find(") ")) {
                        let sub = trimmed[pos + 2..].trim();
                        if !sub.is_empty() {
                            items.push(sub.to_string());
                            continue;
                        }
                    }
                }
            }
            // Check bullet list: "- " or "* "
            if let Some(sub) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                if !sub.trim().is_empty() {
                    items.push(sub.trim().to_string());
                    continue;
                }
            }
        }

        if items.len() >= 2 {
            // Distribute items across available units
            return items
                .into_iter()
                .enumerate()
                .map(|(idx, item)| {
                    let unit = &units[idx % units.len()];
                    let title = if item.chars().count() > 40 {
                        format!("SubTask {}: {}...", idx + 1, item.chars().take(37).collect::<String>())
                    } else {
                        format!("SubTask {}: {}", idx + 1, item)
                    };
                    SubTask {
                        id: format!("subtask_{}", idx + 1),
                        role: unit.role.clone(),
                        title,
                        prompt: format!(
                            "You are the specialized `{}` unit.\nAssigned Subtask:\n{}\n\nOverall Context Brief:\n{}\n\nExecute this subtask thoroughly and provide your output.",
                            unit.role, item, prompt
                        ),
                    }
                })
                .collect();
        }

        // Fallback: Partition by unit roles
        units
            .iter()
            .enumerate()
            .map(|(idx, unit)| {
                SubTask {
                    id: format!("{}_{}", unit.id, idx + 1),
                    role: unit.role.clone(),
                    title: format!("Role Perspective: {}", unit.role),
                    prompt: format!(
                        "You are the specialized `{}` unit.\nFocus Area: {}\nTask Brief:\n{}\n\nExecute the perspective and responsibilities of the `{}` role.",
                        unit.role, unit.role, prompt, unit.role
                    ),
                }
            })
            .collect()
    }
}
