use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use thunder_executor::McpServerConfig;

/// Runtime configuration stored in `.thunder/config.toml` (§2.1).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ThunderConfig {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub sandbox: SandboxSection,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentSection {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            model: default_model(),
            api_base: default_api_base(),
            max_turns: default_max_turns(),
            max_context_tokens: default_max_context_tokens(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxSection {
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for SandboxSection {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout_ms(),
            max_output_bytes: default_max_output_bytes(),
            debounce_ms: default_debounce_ms(),
        }
    }
}

fn default_model() -> String {
    "gpt-4o".to_string()
}
fn default_api_base() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_max_turns() -> usize {
    16
}
fn default_max_context_tokens() -> usize {
    128_000
}
fn default_timeout_ms() -> u64 {
    30_000
}
fn default_max_output_bytes() -> usize {
    65_536
}
fn default_debounce_ms() -> u64 {
    200
}

impl ThunderConfig {
    /// Parse a `.thunder/config.toml` string.
    pub fn parse_toml(content: &str) -> Result<Self, String> {
        toml::from_str(content).map_err(|e| format!("Failed to parse config.toml: {e}"))
    }

    /// Load `.thunder/config.toml` from `workspace_cwd`, falling back to defaults if absent.
    pub async fn load_from_workspace(workspace_cwd: &Path) -> Self {
        let cfg_path = workspace_cwd.join(".thunder").join("config.toml");
        if let Ok(text) = tokio::fs::read_to_string(&cfg_path).await {
            if let Ok(cfg) = Self::parse_toml(&text) {
                return cfg;
            }
        }
        Self::default()
    }

    /// Discover MCP Server configurations from `~/.codex/config.toml` and `~/.claude.json` (§7 Phase 3).
    pub fn parse_ecosystem_mcp_configs(
        codex_toml: Option<&str>,
        claude_json: Option<&str>,
    ) -> BTreeMap<String, McpServerConfig> {
        let mut out = BTreeMap::new();

        // 1. Parse `~/.codex/config.toml` (`[mcp_servers.<name>]`)
        if let Some(toml_str) = codex_toml {
            if let Ok(val) = toml::from_str::<toml::Value>(toml_str) {
                if let Some(servers) = val.get("mcp_servers").and_then(|v| v.as_table()) {
                    for (name, entry) in servers {
                        if let Some(command) = entry.get("command").and_then(|v| v.as_str()) {
                            let args = entry
                                .get("args")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str().map(String::from))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            out.insert(name.clone(), McpServerConfig::new(command).with_args(args));
                        }
                    }
                }
            }
        }

        // 2. Parse `~/.claude.json` (`{ "mcpServers": { "<name>": { "command": "...", "args": [...] } } }`)
        if let Some(json_str) = claude_json {
            if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                if let Some(servers) = val.get("mcpServers").and_then(|v| v.as_object()) {
                    for (name, entry) in servers {
                        if let Some(command) = entry.get("command").and_then(|v| v.as_str()) {
                            let args = entry
                                .get("args")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|x| x.as_str().map(String::from))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            out.entry(name.clone())
                                .or_insert_with(|| McpServerConfig::new(command).with_args(args));
                        }
                    }
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_toml_and_claude_json_ecosystem_mcp_configs() {
        let codex = r#"
[mcp_servers.git]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-git"]
"#;
        let claude = r#"{
  "mcpServers": {
    "sqlite": {
      "command": "uvx",
      "args": ["mcp-server-sqlite", "--db-path", "test.db"]
    }
  }
}"#;
        let merged = ThunderConfig::parse_ecosystem_mcp_configs(Some(codex), Some(claude));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged["git"].command, "npx");
        assert_eq!(merged["sqlite"].command, "uvx");
    }
}
