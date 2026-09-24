//! Role registry: declarative agent roles loaded from JSONL.
//!
//! A *role* is configuration — a persona plus a permission tier — not code.
//! It is authored by the user in plain JSONL and enforced by the Rust host:
//!
//! ```text
//! ~/.thunder/roles.jsonl      # global roles
//! <workspace>/.arp/roles.jsonl  # project roles (same `id` overrides global)
//! ```
//!
//! One line per role. Malformed lines are skipped so a partial write never
//! breaks the whole registry (mirrors `readJsonLines` on the panel side).
//!
//! The permission tier is the load-bearing part: this crate is the authority,
//! never the TypeScript plugin layer.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thunder_agent_loop::types::config::Permission;
use tracing::{debug, warn};

/// Role persona body.
///
/// Accepts either a single string or an array of lines, so authors can keep
/// JSONL's one-object-per-line rule while still writing prompts readably:
///
/// ```json
/// {"id":"plan","persona":["You plan before acting.","Never write files."], ...}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Persona {
    Text(String),
    Lines(Vec<String>),
}

impl Default for Persona {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl Persona {
    /// Flattened persona body, newline-joined for the array form.
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Lines(lines) => lines.join("\n"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.as_text().trim().is_empty()
    }
}

fn default_true() -> bool {
    true
}

/// A declarative agent role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleSpec {
    /// Slash-command id (`/plan`). Unique key; project scope overrides global.
    pub id: String,
    /// Display name shown in the panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Extra slash aliases (e.g. `["p"]` for `/p`).
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// System-prompt body injected ahead of the base prompt.
    #[serde(default)]
    pub persona: Persona,
    /// Tool capability tier. This is what the host enforces.
    #[serde(default)]
    pub permission: Permission,
    /// Optional per-role model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional per-role thinking level override.
    #[serde(
        default,
        rename = "thinkingLevel",
        alias = "thinking_level",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_level: Option<String>,
    /// Whether `ask_user_question` is mounted (enables the panel question bubble).
    #[serde(default, rename = "askUser", alias = "ask_user")]
    pub ask_user: bool,
    /// Whether leaving this role requires explicit user approval.
    #[serde(default, rename = "exitGate", alias = "exit_gate")]
    pub exit_gate: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional keyword auto-selection triggers (when no slash command is used).
    #[serde(default)]
    pub triggers: Vec<String>,
}

impl RoleSpec {
    /// Display name, falling back to the id.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// Whether this role id or one of its aliases matches `token` (case-insensitive).
    pub fn matches(&self, token: &str) -> bool {
        let t = token.trim().trim_start_matches('/');
        self.id.eq_ignore_ascii_case(t)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(t))
    }
}

/// Resolved set of roles, merged across scopes.
#[derive(Debug, Clone, Default)]
pub struct RoleRegistry {
    roles: HashMap<String, RoleSpec>,
    sources: Vec<PathBuf>,
}

impl RoleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load and merge role files. Later sources override earlier ones by `id`.
    pub async fn load_from_sources(sources: Vec<PathBuf>) -> Self {
        let mut registry = Self::new();
        for src in sources {
            registry.load_source(&src).await;
        }
        registry
    }

    /// Load the standard chain: `<workspace>/.arp/roles.jsonl` overrides
    /// `~/.thunder/roles.jsonl`. `<workspace>` wins on conflicting ids.
    pub async fn load_default(workspace: Option<&Path>) -> Self {
        let mut sources: Vec<PathBuf> = Vec::new();
        sources.extend(Self::thunder_home().map(|dir| vec![dir.join("roles.jsonl")]).unwrap_or_default());
        if let Some(ws) = workspace {
            sources.push(ws.join(".arp").join("roles.jsonl"));
        }
        Self::load_from_sources(sources).await
    }

    /// Resolve `$THUNDER_CONFIG_DIR` or `~/.thunder`.
    pub fn thunder_home() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("THUNDER_CONFIG_DIR") {
            if !dir.trim().is_empty() {
                return Some(PathBuf::from(dir));
            }
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".thunder"))
    }

    /// Parse one JSONL file (or a directory of `*.jsonl` files).
    async fn load_source(&mut self, source: &Path) {
        let meta = match tokio::fs::metadata(source).await {
            Ok(m) => m,
            Err(_) => return, // absent scope is normal
        };

        if meta.is_dir() {
            let mut entries: Vec<PathBuf> = match std::fs::read_dir(source) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                    .collect(),
                Err(_) => return,
            };
            entries.sort();
            for entry in entries {
                self.load_file(&entry).await;
            }
            return;
        }

        self.load_file(source).await;
    }

    async fn load_file(&mut self, path: &Path) {
        let raw = match tokio::fs::read_to_string(path).await {
            Ok(r) => r,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "Role file unreadable; skipping");
                return;
            }
        };

        let mut count = 0usize;
        for (idx, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            match serde_json::from_str::<RoleSpec>(trimmed) {
                Ok(role) => {
                    if role.id.trim().is_empty() {
                        warn!(path = %path.display(), line = idx + 1, "Role entry has empty id; skipped");
                        continue;
                    }
                    self.roles.insert(role.id.clone(), role);
                    count += 1;
                }
                Err(err) => {
                    // Tolerate partial trailing writes / bad rows.
                    warn!(path = %path.display(), line = idx + 1, error = %err, "Malformed role line skipped");
                }
            }
        }

        if count > 0 {
            self.sources.push(path.to_path_buf());
            debug!(path = %path.display(), count, "Loaded roles from scope");
        }
    }

    /// Look up by id or alias.
    pub fn resolve(&self, token: &str) -> Option<RoleSpec> {
        self.roles
            .values()
            .find(|r| r.matches(token))
            .cloned()
    }

    pub fn get(&self, id: &str) -> Option<&RoleSpec> {
        self.roles.get(id)
    }

    /// All roles, enabled first, then sorted by id.
    pub fn list(&self) -> Vec<RoleSpec> {
        let mut out: Vec<RoleSpec> = self.roles.values().cloned().collect();
        out.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase()))
        });
        out
    }

    /// Only enabled roles — what a slash-command palette should offer.
    pub fn list_enabled(&self) -> Vec<RoleSpec> {
        self.list().into_iter().filter(|r| r.enabled).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }

    /// Files that contributed at least one role (for reload/debug surfaces).
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        tokio::fs::write(&p, body).await.unwrap();
        p
    }

    #[tokio::test]
    async fn parses_single_line_role_with_string_persona() {
        let dir = tempdir().unwrap();
        let f = write(
            dir.path(),
            "roles.jsonl",
            r#"{"id":"plan","name":"Plan","persona":"You plan first.","permission":"read","askUser":true}"#,
        )
        .await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;

        let role = reg.get("plan").expect("plan role loaded");
        assert_eq!(role.display_name(), "Plan");
        assert_eq!(role.permission, Permission::Read);
        assert!(role.ask_user);
        assert!(role.enabled, "enabled defaults to true");
        assert_eq!(role.persona.as_text(), "You plan first.");
    }

    #[tokio::test]
    async fn alias_only_applies_to_the_role_that_declares_it() {
        let dir = tempdir().unwrap();
        let f = write(
            dir.path(),
            "roles.jsonl",
            concat!(
                r#"{"id":"plan","aliases":["p"],"permission":"read"}"#,
                "\n",
                r#"{"id":"writer","permission":"write"}"#,
                "\n"
            ),
        )
        .await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;

        assert_eq!(reg.resolve("plan").unwrap().id, "plan");
        assert_eq!(reg.resolve("/p").unwrap().id, "plan", "alias + leading slash");
        assert_eq!(reg.resolve("P").unwrap().id, "plan", "case-insensitive");
        assert!(reg.resolve("writer").unwrap().aliases.is_empty());
        assert!(reg.resolve("nope").is_none());
    }

    #[tokio::test]
    async fn array_persona_joins_with_newlines() {
        let dir = tempdir().unwrap();
        let f = write(
            dir.path(),
            "roles.jsonl",
            r#"{"id":"plan","persona":["Line one.","Line two."],"permission":"read"}"#,
        )
        .await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;
        assert_eq!(reg.get("plan").unwrap().persona.as_text(), "Line one.\nLine two.");
    }

    #[tokio::test]
    async fn malformed_lines_are_skipped_without_losing_valid_ones() {
        let dir = tempdir().unwrap();
        let f = write(
            dir.path(),
            "roles.jsonl",
            concat!(
                r#"{"id":"good","permission":"read"}"#,
                "\n",
                r#"{"id":"broken""#,
                "\n",
                "\n",
                r#"{"id":"also_good","permission":"bash"}"#,
                "\n"
            ),
        )
        .await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;

        assert_eq!(reg.len(), 2, "two valid rows survive one bad row");
        assert!(reg.get("good").is_some());
        assert!(reg.get("also_good").is_some());
    }

    #[tokio::test]
    async fn project_scope_overrides_global_by_id() {
        let dir = tempdir().unwrap();
        let global = write(
            dir.path(),
            "global.jsonl",
            r#"{"id":"plan","permission":"bash","name":"Global Plan"}"#,
        )
        .await;
        let project = write(
            dir.path(),
            "project.jsonl",
            r#"{"id":"plan","permission":"read","name":"Project Plan"}"#,
        )
        .await;

        let reg = RoleRegistry::load_from_sources(vec![global, project]).await;
        let role = reg.get("plan").unwrap();
        assert_eq!(role.display_name(), "Project Plan", "later source wins");
        assert_eq!(role.permission, Permission::Read);
        assert_eq!(reg.len(), 1, "same id does not duplicate");
    }

    #[tokio::test]
    async fn permission_defaults_to_bash_when_omitted() {
        let dir = tempdir().unwrap();
        let f = write(dir.path(), "roles.jsonl", r#"{"id":"unscoped"}"#).await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;
        assert_eq!(reg.get("unscoped").unwrap().permission, Permission::Bash);
    }

    #[tokio::test]
    async fn disabled_roles_are_listed_but_excluded_from_enabled() {
        let dir = tempdir().unwrap();
        let f = write(
            dir.path(),
            "roles.jsonl",
            concat!(
                r#"{"id":"on","enabled":true}"#,
                "\n",
                r#"{"id":"off","enabled":false}"#,
                "\n"
            ),
        )
        .await;
        let reg = RoleRegistry::load_from_sources(vec![f]).await;

        assert_eq!(reg.list().len(), 2);
        let enabled = reg.list_enabled();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "on");
    }

    #[tokio::test]
    async fn missing_files_are_not_an_error() {
        let reg = RoleRegistry::load_from_sources(vec![PathBuf::from("/nonexistent/roles.jsonl")]).await;
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn empty_persona_is_detected() {
        assert!(Persona::default().is_empty());
        assert!(!Persona::Lines(vec!["x".into()]).is_empty());
    }
}
