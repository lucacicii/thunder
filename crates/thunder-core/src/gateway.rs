use serde_json::{json, Value};
use std::collections::BTreeSet;
use thunder_protocol::ToolDescriptor;
use thunder_registry::CatalogSnapshot;

/// Meta-tool name for discovering dynamic tools and their schemas (§4.3).
pub const META_DISCOVER_TOOLS: &str = "thunder__discover_tools";

/// Meta-tool name for routing calls to any dynamic (`script__*`, `mcp__*`) or core tool (§4.3).
pub const META_CALL_TOOL: &str = "thunder__call_tool";

/// Front-loaded KV-Cache Shield (§1, §4.3).
///
/// Holds the frozen top-level `tools` list declared at session start so the LLM API request
/// prefix never mutates when scripts or MCP servers are hot-reloaded mid-session.
pub struct KvCacheShield {
    /// Frozen top-level tool declarations sent in every LLM request (`core__*` + 2 meta-tools).
    static_top_level_tools: Vec<ToolDescriptor>,
    /// Snapshot version last observed at a turn boundary.
    last_seen_version: u64,
    /// Dynamic tool names already announced via `<system_notification>`.
    announced_dynamic_tools: BTreeSet<String>,
}

impl KvCacheShield {
    /// Initialize the KV-Cache Shield from the initial `CatalogSnapshot`.
    ///
    /// Only `core__*` tools plus `thunder__discover_tools` and `thunder__call_tool` are placed
    /// in the static top-level `tools` array (§4.3).
    pub fn new(initial_snapshot: &CatalogSnapshot) -> Self {
        let mut static_top_level_tools: Vec<ToolDescriptor> = initial_snapshot
            .descriptors
            .values()
            .filter(|d| d.name.starts_with("core__"))
            .cloned()
            .collect();

        // Ensure deterministic order for 100% KV-Cache prefix stability.
        static_top_level_tools.sort_by(|a, b| a.name.cmp(&b.name));
        static_top_level_tools.push(discover_tools_descriptor());
        static_top_level_tools.push(call_tool_descriptor());

        let announced_dynamic_tools: BTreeSet<String> = initial_snapshot
            .descriptors
            .keys()
            .filter(|name| !name.starts_with("core__"))
            .cloned()
            .collect();

        Self {
            static_top_level_tools,
            last_seen_version: initial_snapshot.version,
            announced_dynamic_tools,
        }
    }

    /// Return the frozen top-level `tools` list for the LLM API request (§4.3).
    pub fn top_level_tools(&self) -> &[ToolDescriptor] {
        &self.static_top_level_tools
    }

    /// Check if `snapshot` has mounted new dynamic tools (`script__*` or `mcp__*`) since the last turn
    /// and generate the lightweight `<system_notification>` block (§4.3).
    pub fn poll_registry_update(&mut self, snapshot: &CatalogSnapshot) -> Option<String> {
        if snapshot.version <= self.last_seen_version {
            return None;
        }
        self.last_seen_version = snapshot.version;

        let mut newly_mounted = Vec::new();
        for (name, desc) in &snapshot.descriptors {
            if name.starts_with("core__") {
                continue;
            }
            if self.announced_dynamic_tools.insert(name.clone()) {
                newly_mounted.push((name.clone(), desc.description.clone()));
            }
        }

        if newly_mounted.is_empty() {
            return None;
        }

        let mut lines = Vec::new();
        lines.push("<system_notification>".to_string());
        for (name, description) in newly_mounted {
            lines.push(format!(
                "[Registry Update] 工具已挂载: `{name}` ({description})。"
            ));
            lines.push(format!(
                "可直接通过 `{META_CALL_TOOL}(name=\"{name}\", arguments={{...}})` 调用。"
            ));
        }
        lines.push("</system_notification>".to_string());
        Some(lines.join("\n"))
    }

    /// Execute the `thunder__discover_tools` meta-tool against the turn's frozen `CatalogSnapshot`.
    pub fn execute_discover_tools(snapshot: &CatalogSnapshot, arguments: &Value) -> String {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        let matched: Vec<&ToolDescriptor> = snapshot
            .descriptors
            .values()
            .filter(|d| {
                query.is_empty()
                    || d.name.to_lowercase().contains(&query)
                    || d.description.to_lowercase().contains(&query)
                    || d.uri.to_lowercase().contains(&query)
            })
            .collect();

        serde_json::to_string_pretty(&matched).unwrap_or_else(|_| "[]".to_string())
    }
}

pub fn discover_tools_descriptor() -> ToolDescriptor {
    ToolDescriptor::new(
        META_DISCOVER_TOOLS,
        "Search or list dynamically mounted MCP and local script tools and retrieve their full JSON Schema.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional keyword or prefix filter (empty returns all registered tools)."
                }
            }
        }),
        "core://discover_tools",
    )
}

pub fn call_tool_descriptor() -> ToolDescriptor {
    ToolDescriptor::new(
        META_CALL_TOOL,
        "Unified gateway router to invoke any dynamically mounted script (`script__*`), MCP (`mcp__*`), or core tool.",
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Target tool name or URI (e.g. 'script__git_squash_feature' or 'mcp__git__commit')."
                },
                "arguments": {
                    "type": "object",
                    "description": "JSON arguments object matching the target tool's input_schema."
                }
            },
            "required": ["name"]
        }),
        "core://call_tool",
    )
}
