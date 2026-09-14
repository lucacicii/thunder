use crate::error::McpError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl McpServerConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            disabled: false,
            cwd: None,
            timeout_ms: None,
        }
    }

    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_cwd(mut self, path: PathBuf) -> Self {
        self.cwd = Some(path);
        self
    }

    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl McpConfig {
    pub fn new() -> Self {
        Self {
            mcp_servers: HashMap::new(),
        }
    }

    pub fn with_server(mut self, name: impl Into<String>, server: McpServerConfig) -> Self {
        self.mcp_servers.insert(name.into(), server);
        self
    }

    /// Parse configuration from a JSON string.
    pub fn parse_json(content: &str) -> Result<Self, McpError> {
        // 1. Try standard `{ "mcpServers": { ... } }` wrapper
        if let Ok(cfg) = serde_json::from_str::<McpConfig>(content) {
            if !cfg.mcp_servers.is_empty() {
                return Ok(cfg);
            }
        }

        // 2. Try raw `{ "server_a": { "command": "..." } }` map
        if let Ok(servers) = serde_json::from_str::<HashMap<String, McpServerConfig>>(content) {
            return Ok(McpConfig { mcp_servers: servers });
        }

        // Return deserialization error
        serde_json::from_str::<McpConfig>(content)
            .map_err(|e| McpError::SerializationError(format!("Failed to parse MCP config JSON: {e}")))
    }

    /// Load configuration from a file.
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self, McpError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(McpError::IoError(format!("MCP config file not found: {}", p.display())));
        }

        let content = tokio::fs::read_to_string(p).await.map_err(|e| {
            McpError::IoError(format!("Failed to read MCP config file {}: {e}", p.display()))
        })?;

        Self::parse_json(&content)
    }

    /// Automatically find and load MCP config file from standard workspace and user locations.
    pub async fn find_and_load_from_workspace(workspace: &Path) -> Option<(PathBuf, Self)> {
        let mut candidates = vec![
            workspace.join("mcp_servers.json"),
            workspace.join(".mcp.json"),
            workspace.join("claude_desktop_config.json"),
            PathBuf::from("mcp_servers.json"),
            PathBuf::from(".mcp.json"),
        ];

        if let Ok(home) = std::env::var("HOME") {
            let home_path = PathBuf::from(home);
            candidates.push(home_path.join(".mcp.json"));
            candidates.push(home_path.join("mcp_servers.json"));
            candidates.push(home_path.join(".cursor/mcp.json"));
            candidates.push(home_path.join(".claude.json"));
            candidates.push(home_path.join("Library/Application Support/Claude/claude_desktop_config.json"));
            candidates.push(home_path.join(".config/claude/claude_desktop_config.json"));
        }

        for path in candidates {
            if path.exists() {
                if let Ok(cfg) = Self::from_file(&path).await {
                    if !cfg.mcp_servers.is_empty() {
                        return Some((path, cfg));
                    }
                }
            }
        }
        None
    }

    /// Render connected MCP servers and commands into a Markdown table.
    pub fn render_servers_markdown(&self, config_path: Option<&Path>) -> String {
        if self.mcp_servers.is_empty() {
            return "No MCP servers configured in current configuration.".to_string();
        }

        let path_info = config_path
            .map(|p| format!(" (Config: `{}`)", p.display()))
            .unwrap_or_default();

        let mut out = format!("### 🔌 Configured MCP Servers{}\n\n", path_info);
        out.push_str("| Server Name | Command | Arguments | Status |\n");
        out.push_str("|---|---|---|---|\n");

        for (name, srv) in &self.mcp_servers {
            let status = if srv.disabled { "⏸ Disabled" } else { "🟢 Enabled" };
            out.push_str(&format!(
                "| **`{}`** | `{}` | `{}` | {} |\n",
                name,
                srv.command,
                srv.args.join(" "),
                status
            ));
        }

        out.push_str("\n*MCP remote tools are discovered dynamically and bridged into AgentLoop as `mcp_<server>_<tool>`.*");
        out
    }
}
