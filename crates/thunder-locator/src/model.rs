use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Source of a proposed workspace candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    /// The currently active workspace directory.
    CurrentWorkspace,
    /// Explicitly registered in ~/.thunder/workspaces.json.
    Registered,
    /// Inferred from thunder-memory (~/.thunder/memory.db) historical samples.
    MemoryHistory,
    /// Inferred from recent Thunder conversation sessions.
    SessionHistory,
}

/// A structured profile for a project workspace stored in ~/.thunder/workspaces.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub canonical_path: PathBuf,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tech_stack: Vec<String>,
    #[serde(default)]
    pub hit_count: u64,
    pub last_used_ms: u64,
    pub created_at_ms: u64,
}

impl WorkspaceRecord {
    pub fn new(canonical_path: PathBuf, name: impl Into<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let n = name.into();
        let id = format!("ws-{}", sanitize_id(&n));
        Self {
            id,
            canonical_path,
            name: n,
            aliases: Vec::new(),
            keywords: Vec::new(),
            tech_stack: Vec::new(),
            hit_count: 0,
            last_used_ms: now,
            created_at_ms: now,
        }
    }

    pub fn with_aliases(mut self, aliases: Vec<String>) -> Self {
        self.aliases = aliases;
        self
    }

    pub fn with_keywords(mut self, keywords: Vec<String>) -> Self {
        self.keywords = keywords;
        self
    }

    pub fn with_tech_stack(mut self, tech_stack: Vec<String>) -> Self {
        self.tech_stack = tech_stack;
        self
    }

    pub fn matches_alias(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        if self.name.to_lowercase().contains(&q) || q.contains(&self.name.to_lowercase()) {
            return true;
        }
        self.aliases
            .iter()
            .any(|a| a.to_lowercase().contains(&q) || q.contains(&a.to_lowercase()))
    }
}

/// A candidate workspace directory presented for selection or confidence ranking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCandidate {
    pub path: PathBuf,
    pub display_name: String,
    pub source: CandidateSource,
    pub confidence: f32,
    pub reason: String,
}

/// Final resolution verdict returned by the WorkspaceLocator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocateVerdict {
    /// High-confidence match: caller should automatically switch workspace and execute.
    DirectMatch {
        target_path: PathBuf,
        workspace_id: Option<String>,
        confidence: f32,
        reason: String,
    },
    /// The prompt targets or is sufficiently relevant to the currently active workspace.
    AlreadyInWorkspace {
        current_path: PathBuf,
        reason: String,
    },
    /// No existing memory or ambiguous candidates: user needs to pick/confirm (Option 3 fast tagging).
    NeedConfirmation {
        prompt: String,
        candidates: Vec<WorkspaceCandidate>,
        reason: String,
    },
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
