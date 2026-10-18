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
    /// Declarative plugin configurations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<PluginSection>,
    /// Web tool configuration (`web_search` / `web_fetch` backends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSection>,
}

#[cfg(feature = "mcp")]
pub type McpServerConfigMap =
    std::collections::HashMap<String, thunder_agent_mcp::config::McpServerConfig>;
#[cfg(not(feature = "mcp"))]
pub type McpServerConfigMap = std::collections::HashMap<String, serde_json::Value>;

/// The `plugins` group of a [`ThunderConfig`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSection {
    /// Explicit whitelist of built-in plugins (e.g. `["read_file", "write_file", "grep"]`).
    /// If `None`, all built-in coding plugins are enabled by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<Vec<String>>,
    /// Blacklist of plugin / tool / interceptor IDs to disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Vec<String>>,
    /// External MCP server configurations mounted as plugins.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "mcp_servers"
    )]
    pub mcp_servers: Option<McpServerConfigMap>,
}

/// The `web` group of a [`ThunderConfig`]: which backend each web tool uses,
/// and the credentials/base URL for the DeepSeek Search/Fetch API.
///
/// Every field is optional so layering keeps working: a project file that only
/// sets `web.fetch.provider` must not blank a user-level `web.deepseek.apiKey`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<WebFetchSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deepseek: Option<DeepSeekSection>,
}

/// `web.search`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchSection {
    /// `free` (default) | `auto` | `deepseek`. Unknown values warn and fall
    /// back to the default rather than aborting the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// `web.fetch`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFetchSection {
    /// `local` (default) | `auto` | `deepseek`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// `web.deepseek`: credentials and transport for the DeepSeek tools API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekSection {
    /// API root, e.g. `https://api.deepseek.com` (a trailing `/v1` is accepted
    /// and stripped). `None` = the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `sk-…` or `${DEEPSEEK_API_KEY}`. `None` = fall back to `auth.json` /
    /// environment / a DeepSeek model already in the provider registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Extra request headers (a proxy or relay that needs a routing header).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, String>>,
    /// Per-request timeout. `None` = the built-in 15s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Retries for 408 / 429 / 5xx. `None` = 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<usize>,
}

/// Which backend `web_search` should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebSearchBackend {
    /// DuckDuckGo, then Exa. No credentials, no billing.
    #[default]
    Free,
    /// DeepSeek when credentials resolve, then the free engines.
    Auto,
    /// DeepSeek only; a missing key is an error, not a silent downgrade.
    DeepSeek,
}

/// Which backend `web_fetch` should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebFetchBackend {
    /// The local fetcher: `reqwest`, HTML→markdown, cross-host redirects
    /// reported to the model. No credentials, no billing.
    #[default]
    Local,
    /// Local first, DeepSeek Fetch when the local attempt fails.
    Auto,
    /// DeepSeek Fetch first, local as the fallback.
    DeepSeek,
}

impl WebSearchBackend {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "free" | "duckduckgo" | "ddg" | "keyless" => Some(Self::Free),
            "auto" => Some(Self::Auto),
            "deepseek" | "deepseek-search" => Some(Self::DeepSeek),
            _ => None,
        }
    }
}

impl WebFetchBackend {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "local" | "native" | "reqwest" => Some(Self::Local),
            "auto" => Some(Self::Auto),
            "deepseek" | "deepseek-fetch" => Some(Self::DeepSeek),
            _ => None,
        }
    }
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
    /// The mode new sessions start in (`plan` / `ask` / `acceptEdits` /
    /// `yolo`; the pre-0.4 names parse too). `None` = `yolo`, which is what
    /// the pre-0.4 default (`never`) asked of nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
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
        paths.push(thunder_agent_loop::core::paths::thunder_config_dir().join(CONFIG_FILE));
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
        if b.default_mode.is_some() {
            self.agent.default_mode = b.default_mode;
        }
        if let Some(p) = other.plugins {
            let entry = self.plugins.get_or_insert_with(PluginSection::default);
            if p.builtin.is_some() {
                entry.builtin = p.builtin;
            }
            if p.disabled.is_some() {
                entry.disabled = p.disabled;
            }
            if p.mcp_servers.is_some() {
                entry.mcp_servers = p.mcp_servers;
            }
        }
        if let Some(w) = other.web {
            let entry = self.web.get_or_insert_with(WebSection::default);
            if let Some(search) = w.search {
                let target = entry.search.get_or_insert_with(WebSearchSection::default);
                if search.provider.is_some() {
                    target.provider = search.provider;
                }
            }
            if let Some(fetch) = w.fetch {
                let target = entry.fetch.get_or_insert_with(WebFetchSection::default);
                if fetch.provider.is_some() {
                    target.provider = fetch.provider;
                }
            }
            if let Some(ds) = w.deepseek {
                let target = entry.deepseek.get_or_insert_with(DeepSeekSection::default);
                if ds.base_url.is_some() {
                    target.base_url = ds.base_url;
                }
                if ds.api_key.is_some() {
                    target.api_key = ds.api_key;
                }
                if ds.headers.is_some() {
                    target.headers = ds.headers;
                }
                if ds.timeout_ms.is_some() {
                    target.timeout_ms = ds.timeout_ms;
                }
                if ds.retries.is_some() {
                    target.retries = ds.retries;
                }
            }
        }
    }

    /// Access the declarative plugin configuration section if present.
    pub fn plugins(&self) -> Option<&PluginSection> {
        self.plugins.as_ref()
    }

    /// Access the web-tool configuration section if present.
    pub fn web(&self) -> Option<&WebSection> {
        self.web.as_ref()
    }

    /// The configured `web_search` backend, defaulting to the free engines.
    ///
    /// An unrecognised value is reported and ignored: a typo must not take the
    /// search tool away, and it must not silently start billing either.
    pub fn search_backend(&self) -> WebSearchBackend {
        let Some(raw) = self
            .web
            .as_ref()
            .and_then(|w| w.search.as_ref())
            .and_then(|s| s.provider.as_deref())
        else {
            return WebSearchBackend::default();
        };
        WebSearchBackend::parse(raw).unwrap_or_else(|| {
            tracing::warn!(
                value = %raw,
                "Unknown web.search.provider; falling back to `free`"
            );
            WebSearchBackend::default()
        })
    }

    /// The configured `web_fetch` backend, defaulting to the local fetcher.
    pub fn fetch_backend(&self) -> WebFetchBackend {
        let Some(raw) = self
            .web
            .as_ref()
            .and_then(|w| w.fetch.as_ref())
            .and_then(|f| f.provider.as_deref())
        else {
            return WebFetchBackend::default();
        };
        WebFetchBackend::parse(raw).unwrap_or_else(|| {
            tracing::warn!(
                value = %raw,
                "Unknown web.fetch.provider; falling back to `local`"
            );
            WebFetchBackend::default()
        })
    }

    /// The DeepSeek credentials/transport section, if any.
    pub fn deepseek(&self) -> Option<&DeepSeekSection> {
        self.web.as_ref().and_then(|w| w.deepseek.as_ref())
    }

    /// Push the config-declared values onto a run's [`AgentConfig`].
    ///
    /// `defaultMode` is read elsewhere: it picks which of the host's approval
    /// modes a new session starts in, and a mode is only ever one of the four
    /// the host ships. There is nothing here that could widen a judgement —
    /// there is no capability tier left to widen.
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
        base.agent.default_mode = Some("plan".to_string());

        let mut overlay = ThunderConfig::default();
        overlay.agent.thinking_level = Some("low".to_string());

        base.merge(overlay);
        assert_eq!(base.agent.max_turns, Some(10), "unset field must survive");
        assert_eq!(base.agent.thinking_level.as_deref(), Some("low"));
        assert_eq!(
            base.agent.default_mode.as_deref(),
            Some("plan"),
            "an overlay that declares no mode must not clear one"
        );

        // ...and one that does declare a mode replaces it, which is the half
        // the daemon reads to seed a new session.
        let mut overlay = ThunderConfig::default();
        overlay.agent.default_mode = Some("ask".to_string());
        base.merge(overlay);
        assert_eq!(base.agent.default_mode.as_deref(), Some("ask"));
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

    #[test]
    fn plugins_section_parses_and_merges() {
        let json_str = r#"{
            "plugins": {
                "builtin": ["read_file", "write_file"],
                "disabled": ["bash"],
                "mcpServers": {
                    "test": {
                        "command": "echo",
                        "args": ["hello"]
                    }
                }
            }
        }"#;

        let mut base: ThunderConfig = serde_json::from_str(json_str).unwrap();
        let plugins = base.plugins().expect("plugins section present");
        assert_eq!(
            plugins.builtin.as_deref(),
            Some(&["read_file".to_string(), "write_file".to_string()][..])
        );
        assert_eq!(plugins.disabled.as_deref(), Some(&["bash".to_string()][..]));

        let overlay_str = r#"{
            "plugins": {
                "disabled": ["bash", "grep"]
            }
        }"#;
        let overlay: ThunderConfig = serde_json::from_str(overlay_str).unwrap();
        base.merge(overlay);

        let merged_plugins = base.plugins().expect("plugins section survives");
        assert_eq!(
            merged_plugins.builtin.as_deref(),
            Some(&["read_file".to_string(), "write_file".to_string()][..])
        );
        assert_eq!(
            merged_plugins.disabled.as_deref(),
            Some(&["bash".to_string(), "grep".to_string()][..])
        );
    }

    #[test]
    fn web_section_merges_field_by_field() {
        let mut base: ThunderConfig = serde_json::from_str(
            r#"{
                "web": {
                    "search": {"provider": "auto"},
                    "deepseek": {"apiKey": "sk-user", "baseUrl": "https://api.deepseek.com"}
                }
            }"#,
        )
        .unwrap();
        let overlay: ThunderConfig =
            serde_json::from_str(r#"{"web":{"fetch":{"provider":"deepseek"}}}"#).unwrap();
        base.merge(overlay);

        // The project layer names only a fetch backend; the user-level key and
        // search backend must survive untouched.
        assert_eq!(base.search_backend(), WebSearchBackend::Auto);
        assert_eq!(base.fetch_backend(), WebFetchBackend::DeepSeek);
        let ds = base.deepseek().expect("the deepseek section survives");
        assert_eq!(ds.api_key.as_deref(), Some("sk-user"));
    }

    #[test]
    fn web_backends_default_to_the_keyless_paths() {
        let cfg = ThunderConfig::default();
        assert_eq!(cfg.search_backend(), WebSearchBackend::Free);
        assert_eq!(cfg.fetch_backend(), WebFetchBackend::Local);
        assert!(cfg.deepseek().is_none());
    }

    #[test]
    fn an_unknown_web_provider_falls_back_to_the_default() {
        // A typo must not remove the tool, and must not start billing.
        let cfg: ThunderConfig =
            serde_json::from_str(r#"{"web":{"search":{"provider":"deepsek"}}}"#).unwrap();
        assert_eq!(cfg.search_backend(), WebSearchBackend::Free);
        let cfg: ThunderConfig =
            serde_json::from_str(r#"{"web":{"fetch":{"provider":"remote"}}}"#).unwrap();
        assert_eq!(cfg.fetch_backend(), WebFetchBackend::Local);
    }
}
