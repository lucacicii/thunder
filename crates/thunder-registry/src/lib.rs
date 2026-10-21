//! Thunder Registry — Lock-free `ArcSwap<CatalogSnapshot>`, TSP v1 Frontmatter parser, and 200ms hot-reload watcher.

pub mod frontmatter;
pub mod registry;
pub mod watcher;

pub use frontmatter::{
    parse_frontmatter_str, parse_script_file, scan_scripts_dir, MAX_FRONTMATTER_SCAN_LINES,
};
pub use registry::{CatalogSnapshot, ToolRegistry};
pub use watcher::{start_hot_reload_watcher, RegistryWatcherHandle, DEFAULT_DEBOUNCE_MS};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;
    use thunder_protocol::{ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider};

    struct StaticProvider {
        id: String,
        tools: Vec<ToolDescriptor>,
        reply: String,
    }

    #[async_trait]
    impl ToolProvider for StaticProvider {
        fn provider_id(&self) -> &str {
            &self.id
        }

        async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
            Ok(self.tools.clone())
        }

        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Value,
            _ctx: &ExecutionContext,
        ) -> Result<String, ToolExecutionError> {
            Ok(self.reply.clone())
        }
    }

    #[tokio::test]
    async fn shadowing_enforces_script_gt_mcp_gt_core() {
        let registry = ToolRegistry::new();

        // 1. Register Core `read_file`
        let core_provider = Arc::new(StaticProvider {
            id: "core".into(),
            tools: vec![ToolDescriptor::new_core(
                "read_file",
                "core read",
                json!({"type": "object"}),
            )],
            reply: "from-core".into(),
        });
        registry.register_provider(core_provider).await.unwrap();

        // 2. Register External MCP `read_file`
        let mcp_provider = Arc::new(StaticProvider {
            id: "mcp:fs".into(),
            tools: vec![ToolDescriptor::new_mcp(
                "fs",
                "read_file",
                "mcp read",
                json!({"type": "object"}),
            )],
            reply: "from-mcp".into(),
        });
        let snap1 = registry.register_provider(mcp_provider).await.unwrap();
        assert_eq!(
            snap1.logical_winners.get("read_file").map(String::as_str),
            Some("mcp__fs__read_file")
        );
        assert!(snap1
            .shadowed_uris
            .contains(&"core://read_file".to_string()));

        // 3. Register Local Script `read_file` -> shadows both MCP and Core
        let dir = tempdir().unwrap();
        let script_path = dir.path().join("read_file.sh");
        let script = r#"#!/usr/bin/env bash
# @thunder
# name: read_file
# description: custom script override for read_file
# @end
echo -n "from-script"
"#;
        tokio::fs::write(&script_path, script).await.unwrap();
        let snap2 = registry.reload_scripts_dir(dir.path()).await.unwrap();

        assert_eq!(
            snap2.logical_winners.get("read_file").map(String::as_str),
            Some("script__read_file")
        );
        assert!(snap2
            .shadowed_uris
            .contains(&"mcp://fs/read_file".to_string()));
        assert!(snap2
            .shadowed_uris
            .contains(&"core://read_file".to_string()));

        let ctx = ExecutionContext::new("sess-shadow", dir.path(), Duration::from_secs(5));
        let out = ToolRegistry::call_with_snapshot(&snap2, "read_file", json!({}), &ctx)
            .await
            .unwrap();
        assert_eq!(out, "from-script");
    }

    #[tokio::test]
    async fn watcher_hot_reloads_new_script_into_arc_swap_snapshot() {
        let dir = tempdir().unwrap();
        let registry = ToolRegistry::new();
        let _watcher = start_hot_reload_watcher(registry.clone(), dir.path().to_path_buf(), None)
            .await
            .expect("start watcher");

        let v0 = registry.current().version;

        let script_path = dir.path().join("git_squash_feature.sh");
        let script = r#"#!/usr/bin/env bash
# @thunder
# name: git_squash_feature
# description: 自动压缩当前分支的所有特性提交并推送到远端
# parameters:
#   type: object
#   properties:
#     base_branch:
#       type: string
#   required: [base_branch]
# @end
echo "squashed onto $(cat)"
"#;
        tokio::fs::write(&script_path, script).await.unwrap();

        // Wait up to 1.5s for the 200ms debounced notify event to fire and swap the snapshot.
        let mut updated = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let snap = registry.current();
            if snap.version > v0 && snap.descriptors.contains_key("script__git_squash_feature") {
                updated = true;
                break;
            }
        }
        assert!(
            updated,
            "ArcSwap<CatalogSnapshot> must hot-reload new script without restart"
        );
    }
}
