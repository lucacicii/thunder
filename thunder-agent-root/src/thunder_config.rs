//! Project & user level `.thunder/config.json` resolution.
//!
//! Thunder reads its agent configuration from a small layered set of
//! `config.json` files, mirroring how the provider registry already merges
//! `~/.thunder` with a project-level `<workspace>/.thunder`:
//!
//! 1. `~/.thunder/config.json`           — user defaults
//! 2. `<ws>/.thunder/config.json`        — project (committed)
//! 3. `<ws>/.thunder/config.local.json`  — project-local (git-ignored)
//!
//! Later layers override earlier ones. **Only fields a file actually declares
//! are merged**: a missing group or field means "unset", never an empty
//! override, so an absent `agent` block cannot silently blank a user default.
//!
//! The schema is intentionally narrow. It exists so a repository can pin the
//! handful of agent behaviours that are genuinely project-scoped (memory
//! files, an appended system-prompt file, turn budget) without re-plumbing
//! every [`AgentConfig`] knob through JSON.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thunder_agent_loop::types::config::AgentConfig;

/// Directory (relative to the workspace root, or `$HOME`) holding thunder's
/// own configuration. Distinct from `.arp`, which belongs to the panel.
pub const THUNDER_DIR: &str = ".thunder";
/// Committed project/user configuration file name.
pub const CONFIG_FILE: &str = "config.json";
/// Git-ignored machine-local override file name.
pub const CONFIG_LOCAL_FILE: &str = "config.local.json";

/// The whole `.thunder/config.json` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThunderConfig {
    /// Schema version. Currently `1`; reserved for future migrations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Agent behaviour overrides.
    #[serde(default)]
    pub agent: AgentSection,
}

/// The `agent` group of a [`ThunderConfig`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSection {
    /// Path to a markdown file whose contents are **appended** to the base
    /// system prompt. Resolved relative to `<ws>/.thunder/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    /// Safety ceiling on autonomous loop turns. `None` = leave the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
    /// Default thinking level forwarded to the transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// Long-term memory configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySection>,
}

/// The `agent.memory` group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySection {
    /// Master switch. Defaults to `true`; a present-but-empty `{}` keeps memory on.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Extra memory files, resolved relative to `<ws>/.thunder/`, appended
    /// after the conventional `THUNDER.md` / `memory/*.md` set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            enabled: true,
            files: None,
        }
    }
}

impl ThunderConfig {
    /// The config files consulted, lowest precedence first.
    pub fn candidate_paths(workspace: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(THUNDER_DIR).join(CONFIG_FILE));
        }
        if let Some(ws) = workspace {
            paths.push(ws.join(THUNDER_DIR).join(CONFIG_FILE));
            paths.push(ws.join(THUNDER_DIR).join(CONFIG_LOCAL_FILE));
        }
        paths
    }

    /// Load and merge every layer that exists. Malformed files are logged and
    /// skipped rather than aborting the run: a broken config must not make the
    /// agent unusable.
    pub async fn load(workspace: Option<&Path>) -> Self {
        let mut merged = Self::default();
        for path in Self::candidate_paths(workspace) {
            let Ok(raw) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            match serde_json::from_str::<Self>(&raw) {
                Ok(cfg) => merged.merge(cfg),
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Ignoring malformed thunder config file"
                    );
                }
            }
        }
        merged
    }

    /// Overlay `other` on top of `self`, field by field. A field is only
    /// replaced when `other` actually declared it.
    pub fn merge(&mut self, other: Self) {
        if other.version.is_some() {
            self.version = other.version;
        }
        let b = other.agent;
        if b.system_prompt_file.is_some() {
            self.agent.system_prompt_file = b.system_prompt_file;
        }
        if b.max_turns.is_some() {
            self.agent.max_turns = b.max_turns;
        }
        if b.thinking_level.is_some() {
            self.agent.thinking_level = b.thinking_level;
        }
        if b.memory.is_some() {
            self.agent.memory = b.memory;
        }
    }

    /// Push the config-declared values onto a run's [`AgentConfig`].
    ///
    /// Deliberately does **not** touch `permission`: the effective capability
    /// tier is owned by the host's approval `SessionPolicy`, and letting a JSON
    /// file widen it here would route around that judgement.
    pub fn apply_to(&self, cfg: &mut AgentConfig) {
        if let Some(turns) = self.agent.max_turns {
            cfg.max_turns = Some(turns);
        }
        if let Some(ref level) = self.agent.thinking_level {
            cfg.thinking_level = Some(level.clone());
        }
    }

    /// The appended system-prompt file, workspace-relative to `<ws>/.thunder/`.
    pub fn system_prompt_file(&self) -> Option<&str> {
        self.agent.system_prompt_file.as_deref()
    }

    /// The memory section, defaulting to "enabled, conventional files only".
    pub fn memory(&self) -> MemorySection {
        self.agent.memory.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_only_overrides_declared_fields() {
        let mut base = ThunderConfig::default();
        base.agent.max_turns = Some(10);
        base.agent.thinking_level = Some("high".to_string());

        let mut overlay = ThunderConfig::default();
        overlay.agent.thinking_level = Some("low".to_string());

        base.merge(overlay);
        assert_eq!(base.agent.max_turns, Some(10), "unset field must survive");
        assert_eq!(base.agent.thinking_level.as_deref(), Some("low"));
    }

    #[test]
    fn memory_defaults_to_enabled() {
        let cfg: ThunderConfig = serde_json::from_str(r#"{"agent":{"memory":{}}}"#).unwrap();
        let mem = cfg.memory();
        assert!(mem.enabled);
        assert!(mem.files.is_none());

        let cfg: ThunderConfig =
            serde_json::from_str(r#"{"agent":{"memory":{"enabled":false}}}"#).unwrap();
        assert!(!cfg.memory().enabled);
    }

    #[test]
    fn camel_case_schema_parses() {
        let cfg: ThunderConfig = serde_json::from_str(
            r#"{"version":1,"agent":{"systemPromptFile":"prompts/system.md","maxTurns":7}}"#,
        )
        .unwrap();
        assert_eq!(cfg.version, Some(1));
        assert_eq!(cfg.system_prompt_file(), Some("prompts/system.md"));
        assert_eq!(cfg.agent.max_turns, Some(7));
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        // Forward compatibility: a newer file with fields we do not know yet
        // must still load the parts we do understand.
        let cfg: ThunderConfig =
            serde_json::from_str(r#"{"agent":{"maxTurns":3,"futureKnob":true}}"#).unwrap();
        assert_eq!(cfg.agent.max_turns, Some(3));
    }
}
