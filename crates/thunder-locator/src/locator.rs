use crate::memory_bridge::MemoryBridge;
use crate::model::{CandidateSource, LocateVerdict, WorkspaceCandidate, WorkspaceRecord};
use crate::registry::WorkspaceRegistry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::debug;

pub struct WorkspaceLocator {
    registry: WorkspaceRegistry,
    memory_bridge: MemoryBridge,
    direct_match_threshold: f32,
}

impl WorkspaceLocator {
    pub fn new(registry: WorkspaceRegistry, memory_bridge: MemoryBridge) -> Self {
        Self {
            registry,
            memory_bridge,
            direct_match_threshold: 0.75,
        }
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.direct_match_threshold = threshold;
        self
    }

    pub fn registry(&self) -> &WorkspaceRegistry {
        &self.registry
    }

    pub fn memory_bridge(&self) -> &MemoryBridge {
        &self.memory_bridge
    }

    /// Primary entry point: evaluate prompt against current workspace and memory.
    pub async fn locate(&self, prompt: &str, current_workspace: &Path) -> LocateVerdict {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return LocateVerdict::AlreadyInWorkspace {
                current_path: current_workspace.to_path_buf(),
                reason: "Empty prompt, stay in current workspace".to_string(),
            };
        }

        let canonical_current = current_workspace
            .canonicalize()
            .unwrap_or_else(|_| current_workspace.to_path_buf());

        let tokens = extract_tokens(trimmed);
        debug!("Extracted locator tokens: {:?}", tokens);

        let registered = self.registry.list().await;

        // Map canonical path -> (Score, Candidate)
        let mut scored_map: HashMap<PathBuf, (f32, WorkspaceCandidate)> = HashMap::new();

        // 1. Score against explicit registered profiles
        for rec in &registered {
            if !rec.canonical_path.is_dir() {
                continue;
            }
            let (score, reason) = score_registered_record(rec, trimmed, &tokens);
            if score > 0.0 {
                let candidate = WorkspaceCandidate {
                    path: rec.canonical_path.clone(),
                    display_name: rec.name.clone(),
                    source: CandidateSource::Registered,
                    confidence: score,
                    reason,
                };
                scored_map.insert(rec.canonical_path.clone(), (score, candidate));
            }
        }

        // 2. Score against thunder-memory historical samples
        let memory_hits = self.memory_bridge.search_project_associations(&tokens, 5);
        for (path, hits) in memory_hits {
            if !path.is_dir() {
                continue;
            }
            let canonical = path.canonicalize().unwrap_or(path.clone());
            let mem_score = (hits as f32 * 0.15).min(0.70);
            let display_name = canonical
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("project")
                .to_string();

            if let Some((existing_score, cand)) = scored_map.get_mut(&canonical) {
                *existing_score = (*existing_score + mem_score * 0.4).min(1.0);
                cand.confidence = *existing_score;
                cand.reason = format!("{}, thunder-memory history ({} hits)", cand.reason, hits);
            } else {
                let candidate = WorkspaceCandidate {
                    path: canonical.clone(),
                    display_name,
                    source: CandidateSource::MemoryHistory,
                    confidence: mem_score,
                    reason: format!("thunder-memory history ({} hits)", hits),
                };
                scored_map.insert(canonical, (mem_score, candidate));
            }
        }

        // 3. Evaluate current workspace affinity vs cross-project intention
        let mut candidates: Vec<WorkspaceCandidate> =
            scored_map.into_values().map(|(_, cand)| cand).collect();

        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Check top candidate
        if let Some(top) = candidates.first() {
            if top.path == canonical_current {
                return LocateVerdict::AlreadyInWorkspace {
                    current_path: canonical_current,
                    reason: format!(
                        "Matched current workspace '{}' (confidence: {:.2})",
                        top.display_name, top.confidence
                    ),
                };
            }

            if top.confidence >= self.direct_match_threshold {
                let ws_id = registered
                    .iter()
                    .find(|w| w.canonical_path == top.path)
                    .map(|w| w.id.clone());

                return LocateVerdict::DirectMatch {
                    target_path: top.path.clone(),
                    workspace_id: ws_id,
                    confidence: top.confidence,
                    reason: top.reason.clone(),
                };
            }
        }

        // 4. No memory found or confidence too low: Build Option 3 fast tagging candidate list
        let mut fallback_candidates = Vec::new();

        // Slot 1: Current workspace (Recommended)
        let curr_name = canonical_current
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("current")
            .to_string();
        fallback_candidates.push(WorkspaceCandidate {
            path: canonical_current.clone(),
            display_name: format!("{curr_name} (当前工作区)"),
            source: CandidateSource::CurrentWorkspace,
            confidence: 0.5,
            reason: "当前活跃工作区".to_string(),
        });

        // Slot 2+: Top candidates or top memory projects
        for cand in candidates.into_iter().take(3) {
            if cand.path != canonical_current
                && !fallback_candidates.iter().any(|c| c.path == cand.path)
            {
                fallback_candidates.push(cand);
            }
        }

        // If still fewer than 3 candidates, pull recent top paths from thunder-memory
        if fallback_candidates.len() < 3 {
            let top_mem = self.memory_bridge.query_top_project_paths(4);
            for (p, _) in top_mem {
                let can = p.canonicalize().unwrap_or(p);
                if can != canonical_current && !fallback_candidates.iter().any(|c| c.path == can) {
                    let d_name = can
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("project")
                        .to_string();
                    fallback_candidates.push(WorkspaceCandidate {
                        path: can,
                        display_name: format!("{d_name} (历史高频)"),
                        source: CandidateSource::MemoryHistory,
                        confidence: 0.3,
                        reason: "thunder-memory 历史活跃项目".to_string(),
                    });
                    if fallback_candidates.len() >= 4 {
                        break;
                    }
                }
            }
        }

        LocateVerdict::NeedConfirmation {
            prompt: trimmed.to_string(),
            candidates: fallback_candidates,
            reason: "未找到高置信度的相关工作区记忆".to_string(),
        }
    }
}

/// Tokenize and extract significant keywords from user prompt.
pub fn extract_tokens(prompt: &str) -> Vec<String> {
    let stop_words: HashSet<&str> = [
        "修复", "修改", "查看", "优化", "重构", "编写", "增加", "实现", "帮忙", "帮我", "在", "和",
        "与", "到", "将", "把", "是", "的", "了", "有", "么", "吗", "呢", "一个", "一下", "怎样",
        "怎么", "如何", "这个", "那个", "项目", "代码", "工程", "目录", "请", "想", "要", "fix",
        "update", "check", "the", "a", "an", "in", "to", "for", "of", "and", "or", "on", "with",
    ]
    .into_iter()
    .collect();

    let mut tokens = Vec::new();
    let lower = prompt.to_lowercase();

    // 1. Extract alphanumeric words / identifiers (e.g. "wb-auth", "agent-resume-panel", "anfeng")
    let mut current_word = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            current_word.push(ch);
        } else {
            if current_word.len() >= 2 && !stop_words.contains(current_word.as_str()) {
                tokens.push(current_word.clone());
            }
            current_word.clear();
        }
    }
    if current_word.len() >= 2 && !stop_words.contains(current_word.as_str()) {
        tokens.push(current_word);
    }

    // 2. Extract Chinese bigrams/trigrams (e.g. "运单", "仓储", "安丰", "分红")
    let cjk_chars: Vec<char> = prompt
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fa5}').contains(c))
        .collect();

    if cjk_chars.len() >= 2 {
        for i in 0..cjk_chars.len() - 1 {
            let bigram: String = cjk_chars[i..=i + 1].iter().collect();
            if !stop_words.contains(bigram.as_str()) && !tokens.contains(&bigram) {
                tokens.push(bigram);
            }
        }
    }

    tokens
}

fn score_registered_record(
    rec: &WorkspaceRecord,
    raw_prompt: &str,
    tokens: &[String],
) -> (f32, String) {
    let lower_prompt = raw_prompt.to_lowercase();
    let name_lower = rec.name.to_lowercase();

    // 1. Direct name match (e.g. prompt contains "anfeng")
    if lower_prompt.contains(&name_lower) {
        return (0.95, format!("精确命中项目名 '{}'", rec.name));
    }

    // 2. Alias match
    for alias in &rec.aliases {
        let alias_lower = alias.to_lowercase();
        if lower_prompt.contains(&alias_lower) {
            return (0.90, format!("命中项目别名 '{}'", alias));
        }
    }

    // 3. Keyword / tech stack overlap
    let mut match_count = 0;
    let mut matched_terms = Vec::new();

    for kw in &rec.keywords {
        let kw_lower = kw.to_lowercase();
        if lower_prompt.contains(&kw_lower) || tokens.iter().any(|t| t == &kw_lower) {
            match_count += 1;
            matched_terms.push(kw.clone());
        }
    }

    for tech in &rec.tech_stack {
        let tech_lower = tech.to_lowercase();
        if lower_prompt.contains(&tech_lower) || tokens.iter().any(|t| t == &tech_lower) {
            match_count += 1;
            matched_terms.push(tech.clone());
        }
    }

    if match_count > 0 {
        let score = (0.60 + (match_count as f32 * 0.15)).min(0.90);
        return (
            score,
            format!("命中画像关键词: [{}]", matched_terms.join(", ")),
        );
    }

    (0.0, String::new())
}
